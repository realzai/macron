//! The web UI's rules, checked against the router in-process: what each request gets, before and
//! after signing in. End-to-end use of the pages is in `tests/web.rs`.

use std::ffi::OsString;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use tempfile::TempDir;
use tokio::sync::watch;
use tower::ServiceExt;

use super::{AppState, router};
use crate::launchd::Launchctl;
use crate::ops::Macron;
use crate::paths::Paths;

const PORT: u16 = 6220;

struct Fixture {
    _dir: TempDir,
    _stop: watch::Sender<bool>,
    router: Router,
    token: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_owned();
        let paths = Paths::resolve(|key| match key {
            "HOME" => Some(OsString::from(&root)),
            "MACRON_CONFIG_DIR" => Some(root.join("config").into()),
            "MACRON_STATE_DIR" => Some(root.join("state").into()),
            "MACRON_LAUNCH_AGENTS_DIR" => Some(root.join("agents").into()),
            _ => None,
        })
        .unwrap();
        // With no jobs, nothing here ever calls launchctl.
        let macron = Macron::new(
            paths,
            Launchctl::from_env(),
            PathBuf::from("/usr/local/bin/macron"),
        );
        let token = "ab".repeat(32);
        let (stop, stopping) = watch::channel(false);
        let router = router(AppState::new(macron, token.clone(), PORT, stopping));
        Self {
            _dir: dir,
            _stop: stop,
            router,
            token,
        }
    }

    fn cookie(&self) -> String {
        format!("macron-session={}", self.token)
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, headers, String::from_utf8(body.to_vec()).unwrap())
    }

    /// A GET from a signed-in browser.
    async fn get(&self, path: &str) -> (StatusCode, HeaderMap, String) {
        self.send(
            request(Method::GET, path)
                .header(header::COOKIE, self.cookie())
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }
}

fn request(method: Method, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, format!("127.0.0.1:{PORT}"))
}

#[tokio::test]
async fn serves_pages_to_a_signed_in_browser() {
    let fixture = Fixture::new();
    let (status, headers, body) = fixture.get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert!(body.contains("No jobs yet"));
}

#[tokio::test]
async fn refuses_a_request_for_another_host() {
    let fixture = Fixture::new();
    for host in ["evil.example:6220", "127.0.0.1:9999", "127.0.0.1"] {
        let request = Request::builder()
            .uri("/")
            .header(header::HOST, host)
            .header(header::COOKIE, fixture.cookie())
            .body(Body::empty())
            .unwrap();
        let (status, _, _) = fixture.send(request).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{host}");
    }
    let localhost = Request::builder()
        .uri("/")
        .header(header::HOST, format!("localhost:{PORT}"))
        .header(header::COOKIE, fixture.cookie())
        .body(Body::empty())
        .unwrap();
    assert_eq!(fixture.send(localhost).await.0, StatusCode::OK);
}

#[tokio::test]
async fn requires_signing_in() {
    let fixture = Fixture::new();
    let anonymous = request(Method::GET, "/").body(Body::empty()).unwrap();
    let (status, _, body) = fixture.send(anonymous).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("macron up"));

    let forged = request(Method::GET, "/jobs/new")
        .header(
            header::COOKIE,
            format!("macron-session={}", "cd".repeat(32)),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(fixture.send(forged).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn signs_in_with_the_token_alone() {
    let fixture = Fixture::new();
    let signed_in = request(Method::GET, &format!("/auth?token={}", fixture.token))
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = fixture.send(signed_in).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/");
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.starts_with(&fixture.cookie()));
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));

    for path in ["/auth?token=wrong", "/auth"] {
        let refused = request(Method::GET, path).body(Body::empty()).unwrap();
        let (status, headers, _) = fixture.send(refused).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert!(headers.get(header::SET_COOKIE).is_none());
    }
}

#[tokio::test]
async fn takes_changes_only_from_its_own_pages() {
    let fixture = Fixture::new();
    let post = |origin: Option<&str>| {
        let mut builder = request(Method::POST, "/apply").header(header::COOKIE, fixture.cookie());
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        builder.body(Body::empty()).unwrap()
    };
    for origin in [
        None,
        Some("http://127.0.0.1:3000"),
        Some("https://evil.example"),
        Some("null"),
    ] {
        assert_eq!(
            fixture.send(post(origin)).await.0,
            StatusCode::FORBIDDEN,
            "{origin:?}"
        );
    }
    let own = post(Some(&format!("http://127.0.0.1:{PORT}")));
    let (status, headers, _) = fixture.send(own).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers[header::LOCATION], "/");
}

#[tokio::test]
async fn forbids_framing_and_outside_scripts() {
    let fixture = Fixture::new();
    for path in ["/", "/nowhere"] {
        let (_, headers, _) = fixture.get(path).await;
        let policy = headers[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(policy.contains("default-src 'self'"), "{path}");
        assert!(policy.contains("frame-ancestors 'none'"), "{path}");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    }
    let refused = request(Method::GET, "/").body(Body::empty()).unwrap();
    let (_, headers, _) = fixture.send(refused).await;
    assert!(headers.contains_key(header::CONTENT_SECURITY_POLICY));
}

#[tokio::test]
async fn serves_its_stylesheet_and_script_without_signing_in() {
    let fixture = Fixture::new();
    for (path, content_type) in [
        ("/assets/app.css", "text/css; charset=utf-8"),
        ("/assets/app.js", "text/javascript; charset=utf-8"),
    ] {
        let anonymous = request(Method::GET, path).body(Body::empty()).unwrap();
        let (status, headers, _) = fixture.send(anonymous).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(headers[header::CONTENT_TYPE], content_type);
    }
    let missing = request(Method::GET, "/assets/nope.js")
        .body(Body::empty())
        .unwrap();
    assert_eq!(fixture.send(missing).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn answers_an_unknown_address_with_a_page() {
    let fixture = Fixture::new();
    let (status, _, body) = fixture.get("/nowhere").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("nothing at this address"));
    assert_eq!(fixture.get("/jobs/nope").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn describes_a_schedule_as_it_is_typed() {
    let fixture = Fixture::new();
    let (status, _, body) = fixture.get("/schedule?text=0%209%20*%20*%20*").await;
    assert_eq!(status, StatusCode::OK);
    let described: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(described["description"], "at 09:00 every day");
    assert_eq!(described["next_runs"].as_array().unwrap().len(), 3);

    let (_, _, body) = fixture.get("/schedule?text=0%2025%20*%20*%20*").await;
    let described: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(described["error"], "hour: `25` is out of range; use 0–23");
}
