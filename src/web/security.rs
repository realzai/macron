//! What keeps the web UI to you, and not to every website you visit.
//!
//! The UI can create jobs, and a job runs any command as you, so a page elsewhere that could reach
//! it could run anything. Four rules stop that:
//!
//! - **Loopback only.** The server listens on 127.0.0.1, so nothing off this Mac can connect.
//! - **The host must be this server.** A request whose `Host` is not `127.0.0.1:<port>` or
//!   `localhost:<port>` is refused. That stops DNS rebinding, where a site points its own name at
//!   127.0.0.1: the browser still sends the site's name as the host.
//! - **Signed in.** Every page needs the session cookie, which only `/auth?token=<secret>` sets. The
//!   secret lives in the state directory, readable by you alone, and `macron up` opens the link.
//! - **Changes come from this server's own pages.** A request that changes anything must be a POST
//!   whose `Origin` is this server. The cookie is `SameSite=Strict`, but a page on another port of
//!   127.0.0.1 counts as the same site, so this check is what stops one.
//!
//! Responses also forbid being framed, so another page cannot overlay the UI and trick clicks into
//! it, and allow no scripts or styles but the UI's own.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;

use super::{AppState, pages};
use crate::error::{Error, Result};
use crate::paths::Paths;

const COOKIE: &str = "macron-session";

/// How long a browser stays signed in, so a bookmark keeps working across browser restarts.
/// `macron up --rotate-token` signs every browser out sooner.
const SESSION_SECONDS: u32 = 30 * 24 * 60 * 60;

/// Every response's policy: only this server's scripts, styles, images and forms, and no framing.
const CONTENT_SECURITY_POLICY: &str =
    "default-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'";

/// The secret that signs a browser in: created on first use, or replaced when `rotate` is set, and
/// kept in the state directory, readable by you alone.
pub(super) fn token(paths: &Paths, rotate: bool) -> Result<String> {
    let path = paths.state_dir.join("web-token");
    if !rotate
        && let Ok(text) = fs::read_to_string(&path)
        && is_token(text.trim())
    {
        return Ok(text.trim().to_owned());
    }
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut bytes))
        .map_err(Error::io("read", "/dev/urandom"))?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    fs::create_dir_all(&paths.state_dir).map_err(Error::io("create", &paths.state_dir))?;
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| {
            // `mode` applies only when the file is created; an older one may be more open.
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(token.as_bytes())
        })
        .map_err(Error::io("write", &path))?;
    Ok(token)
}

fn is_token(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The link that signs a browser in to the UI on `port`.
pub(super) fn sign_in_url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/auth?token={token}")
}

/// Applies the rules in the module documentation to every request.
pub(super) async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let port = state.inner.port;
    if !is_own_host(request.headers(), port) {
        return refuse(
            StatusCode::FORBIDDEN,
            "This page was asked for under another name. Open macron at 127.0.0.1.",
        );
    }
    let path = request.uri().path();
    let public = path == "/auth" || path.starts_with("/assets/");
    if !public && !is_signed_in(request.headers(), &state.inner.token) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "Run `macron up` in a terminal. It opens this page and signs you in.",
        );
    }
    let reads_only = matches!(*request.method(), Method::GET | Method::HEAD);
    if !reads_only && !is_own_origin(request.headers(), port) {
        return refuse(
            StatusCode::FORBIDDEN,
            "Changes can only be made from macron's own pages.",
        );
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

#[derive(Deserialize)]
pub(super) struct SignIn {
    token: Option<String>,
}

/// `/auth?token=<secret>`: with the right secret, sets the session cookie and goes to the jobs.
pub(super) async fn sign_in(
    State(state): State<AppState>,
    Query(sign_in): Query<SignIn>,
) -> Response {
    let token = &state.inner.token;
    let valid = sign_in
        .token
        .is_some_and(|given| same(given.as_bytes(), token.as_bytes()));
    if !valid {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "That sign-in link is out of date. Run `macron up` in a terminal for a new one.",
        );
    }
    let cookie =
        format!("{COOKIE}={token}; Path=/; Max-Age={SESSION_SECONDS}; HttpOnly; SameSite=Strict");
    let mut response = Redirect::to("/").into_response();
    if let Ok(cookie) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

fn is_own_host(headers: &HeaderMap, port: u16) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok());
    host.is_some_and(|host| {
        host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
    })
}

fn is_own_origin(headers: &HeaderMap, port: u16) -> bool {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|origin| origin.to_str().ok());
    origin.is_some_and(|origin| {
        origin == format!("http://127.0.0.1:{port}") || origin == format!("http://localhost:{port}")
    })
}

fn is_signed_in(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| name == COOKIE && same(value.as_bytes(), token.as_bytes()))
}

/// Compares secrets in time that does not depend on where they first differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |differ, (x, y)| differ | (x ^ y)) == 0
}

fn refuse(status: StatusCode, message: &str) -> Response {
    let (title, back) = if status == StatusCode::UNAUTHORIZED {
        ("Sign in through macron", false)
    } else {
        ("Not allowed", true)
    };
    let mut response = match pages::error_page(title, message, back) {
        Ok(page) => (status, Html(page)).into_response(),
        Err(_) => (status, message.to_owned()).into_response(),
    };
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_secrets_whole() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"ab"));
    }

    #[test]
    fn recognises_a_token() {
        assert!(is_token(&"a1".repeat(32)));
        assert!(!is_token("short"));
        assert!(!is_token(&"zz".repeat(32)));
    }
}
