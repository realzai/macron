//! The web UI that `macron up` serves on 127.0.0.1.
//!
//! It is a client of [`Macron`] just as the CLI is: pages show what `Macron` returns and forms call
//! the same operations. Pages are rendered on the server from `templates/`, and every one works
//! without JavaScript. `assets/app.js` adds what has to be live: the jobs list refreshing itself, a
//! running job's output arriving, and the schedule described as you type it.
//!
//! What keeps the page to you, and not to every website you visit, is in [`security`].

mod assets;
mod forms;
mod pages;
mod runs;
mod security;

use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::error::{Error, Result};
use crate::history::process_exists;
use crate::ops::Macron;
use crate::paths::Paths;

/// The port `macron up` tries first, so a bookmark keeps working. If it is taken, the system picks
/// a free one.
pub const DEFAULT_PORT: u16 = 6220;

/// How `macron up` was asked to run.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// The port to listen on. Without one, [`DEFAULT_PORT`], or a free port if that is taken.
    pub port: Option<u16>,
    /// Whether to open the page in the browser.
    pub open: bool,
    /// Whether to replace the sign-in secret, signing out every browser.
    pub rotate_token: bool,
}

/// Serves the web UI until Ctrl-C. If it is already being served, opens that instead.
pub fn up(macron: Macron, options: Options) -> Result<()> {
    let paths = macron.paths().clone();
    if let Some(instance) = Instance::running(&paths) {
        if options.rotate_token {
            return Err(Error::AlreadyUp {
                port: instance.port,
            });
        }
        let token = security::token(&paths, false)?;
        println!("macron is already up at {}", base_url(instance.port));
        if options.open {
            open_browser(&security::sign_in_url(instance.port, &token));
        }
        return Ok(());
    }
    let token = security::token(&paths, options.rotate_token)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(Error::io("start", "the web server"))?;
    runtime.block_on(serve(macron, token, options))
}

async fn serve(macron: Macron, token: String, options: Options) -> Result<()> {
    let listener = bind(options.port).await?;
    let port = listener
        .local_addr()
        .map_err(Error::io("listen on", "127.0.0.1"))?
        .port();
    let paths = macron.paths().clone();
    let instance = Instance::record(&paths, port)?;

    let sign_in = security::sign_in_url(port, &token);
    println!("macron is up at {}", base_url(port));
    if options.open {
        println!("Opening it in your browser. If it does not open, this link signs you in:");
        open_browser(&sign_in);
    } else {
        println!("This link signs you in:");
    }
    println!("  {sign_in}");
    println!("Press Ctrl-C to stop.");

    let (stop, stopping) = watch::channel(false);
    let app = router(AppState::new(macron, token, port, stopping));
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // Live log streams would hold the server open; this tells them to end.
            let _ = stop.send(true);
        })
        .await;
    instance.remove();
    served.map_err(Error::io("serve on", base_url(port)))
}

async fn bind(port: Option<u16>) -> Result<TcpListener> {
    let listen = |port: u16| TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    match port {
        Some(port) => listen(port).await.map_err(|err| match err.kind() {
            io::ErrorKind::AddrInUse => Error::PortInUse(port),
            _ => Error::io("listen on", format!("127.0.0.1:{port}"))(err),
        }),
        None => match listen(DEFAULT_PORT).await {
            Ok(listener) => Ok(listener),
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
                listen(0).await.map_err(Error::io("listen on", "127.0.0.1"))
            }
            Err(err) => Err(Error::io("listen on", format!("127.0.0.1:{DEFAULT_PORT}"))(
                err,
            )),
        },
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let terminate = async {
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                terminate.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = terminate => {}
    }
}

fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn open_browser(url: &str) {
    let opened = Command::new("/usr/bin/open").arg(url).status();
    if !matches!(opened, Ok(status) if status.success()) {
        eprintln!("macron: could not open a browser; open the link above yourself");
    }
}

/// The `macron up` that is running, as recorded in `state/up.json`.
#[derive(Debug, Serialize, Deserialize)]
struct Instance {
    pid: u32,
    port: u16,
    #[serde(skip)]
    path: PathBuf,
}

impl Instance {
    fn path(paths: &Paths) -> PathBuf {
        paths.state_dir.join("up.json")
    }

    /// The instance recorded, if its process is alive and its port answers.
    fn running(paths: &Paths) -> Option<Self> {
        let text = fs::read_to_string(Self::path(paths)).ok()?;
        let instance: Self = serde_json::from_str(&text).ok()?;
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, instance.port).into();
        let answers = TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok();
        (process_exists(instance.pid) && answers).then_some(instance)
    }

    fn record(paths: &Paths, port: u16) -> Result<Self> {
        let instance = Self {
            pid: std::process::id(),
            port,
            path: Self::path(paths),
        };
        if let Some(dir) = instance.path.parent() {
            fs::create_dir_all(dir).map_err(Error::io("create", dir))?;
        }
        let json = serde_json::to_string(&instance)?;
        fs::write(&instance.path, json).map_err(Error::io("write", &instance.path))?;
        Ok(instance)
    }

    /// Removes the record, unless another `macron up` has written its own since.
    fn remove(&self) {
        let ours = fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .is_some_and(|recorded| recorded.pid == self.pid);
        if ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// What every handler can reach.
#[derive(Clone)]
pub(crate) struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    macron: Macron,
    token: String,
    port: u16,
    stopping: watch::Receiver<bool>,
}

impl AppState {
    fn new(macron: Macron, token: String, port: u16, stopping: watch::Receiver<bool>) -> Self {
        Self {
            inner: Arc::new(Inner {
                macron,
                token,
                port,
                stopping,
            }),
        }
    }

    fn paths(&self) -> &Paths {
        self.inner.macron.paths()
    }

    /// Runs `operation` on a thread where blocking is fine: everything `Macron` does reads files,
    /// runs `launchctl` or queries SQLite.
    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Macron) -> Result<T> + Send + 'static,
    ) -> Result<T, PageError> {
        self.try_run(operation).await.map_err(PageError::from)
    }

    /// [`run`](Self::run), keeping the error as it is, for a form to tie to its fields.
    async fn try_run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Macron) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let inner = Arc::clone(&self.inner);
        match tokio::task::spawn_blocking(move || operation(&inner.macron)).await {
            Ok(result) => result,
            Err(err) => Err(Error::Internal(format!("the request failed: {err}"))),
        }
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(pages::jobs))
        .route("/jobs", post(pages::create))
        .route("/jobs/new", get(pages::new_job))
        .route("/jobs/{name}", get(pages::job).post(pages::update))
        .route("/jobs/{name}/edit", get(pages::edit_job))
        .route("/jobs/{name}/run", post(pages::run_now))
        .route("/jobs/{name}/enable", post(pages::enable))
        .route("/jobs/{name}/disable", post(pages::disable))
        .route(
            "/jobs/{name}/delete",
            get(pages::confirm_delete).post(pages::delete),
        )
        .route("/jobs/{name}/runs/{id}", get(runs::run))
        .route("/jobs/{name}/runs/{id}/events", get(runs::events))
        .route("/runs", get(runs::runs))
        .route("/apply", post(pages::apply))
        .route("/schedule", get(pages::describe_schedule))
        .route("/assets/{file}", get(assets::serve))
        .route("/auth", get(security::sign_in))
        .fallback(pages::not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::guard,
        ))
        .with_state(state)
}

/// An error shown as a page, with a status that fits it.
pub(crate) struct PageError {
    status: StatusCode,
    message: String,
}

impl PageError {
    fn internal(message: String) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    }
}

impl From<Error> for PageError {
    fn from(err: Error) -> Self {
        let status = match err.code() {
            "no_such_job" | "no_such_run" | "no_runs" | "not_found" => StatusCode::NOT_FOUND,
            "invalid_schedule" | "invalid_duration" | "invalid_job_name" | "invalid_job" => {
                StatusCode::BAD_REQUEST
            }
            "job_exists" | "job_disabled" | "not_loaded" | "already_running" => {
                StatusCode::CONFLICT
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: err.to_string(),
        }
    }
}

impl From<askama::Error> for PageError {
    fn from(err: askama::Error) -> Self {
        Self::internal(format!("cannot render the page: {err}"))
    }
}

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        let title = match self.status {
            StatusCode::NOT_FOUND => "Not found",
            StatusCode::INTERNAL_SERVER_ERROR => "Something went wrong",
            _ => "That did not work",
        };
        match pages::error_page(title, &self.message, true) {
            Ok(page) => (self.status, Html(page)).into_response(),
            Err(_) => (self.status, self.message).into_response(),
        }
    }
}

/// `path` with the home directory written as `~`, for showing on a page.
fn tilde(path: &Path, paths: &Paths) -> String {
    crate::human::tilde(path, &paths.home)
}

#[cfg(test)]
mod tests;
