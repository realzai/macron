use std::io;
use std::path::PathBuf;

/// Every failure macron reports.
///
/// [`Error::code`] is part of the JSON contract: agents match on it, so a released code never
/// changes meaning. Add a variant rather than reusing a code for something new.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot find your home directory: HOME is not set")]
    NoHome,

    #[error("{var} must be an absolute path, got {value:?}")]
    RelativePath { var: &'static str, value: PathBuf },

    #[error("invalid schedule {schedule:?}: {reason}")]
    InvalidSchedule { schedule: String, reason: String },

    #[error("invalid duration {value:?}: {reason}")]
    InvalidDuration { value: String, reason: String },

    #[error(
        "invalid job name {0:?}: use lowercase letters, digits, `-` and `_`, starting with a \
         letter or digit, up to 64 characters"
    )]
    InvalidJobName(String),

    #[error("invalid job `{job}`: {reason}")]
    InvalidJob { job: String, reason: String },

    #[error("there is already a job named `{0}`")]
    JobExists(String),

    #[error("there is no job named `{0}`")]
    NoSuchJob(String),

    #[error("invalid {}: {reason}", path.display())]
    InvalidConfig { path: PathBuf, reason: String },

    #[error("cannot {action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },

    #[error("`{command}` exited with status {status}{detail}")]
    Launchctl {
        command: String,
        status: i32,
        /// What `launchctl` printed to stderr, as `": <message>"`, or empty.
        detail: String,
    },

    #[error("cannot start job `{job}`: {reason}")]
    Spawn { job: String, reason: String },

    #[error("{0} needs --yes when there is no terminal to confirm on")]
    ConfirmationRequired(String),

    #[error("`{0}` has not run yet")]
    NoRuns(String),

    #[error("`{job}` has no run {run}")]
    NoSuchRun { job: String, run: i64 },

    #[error("`{0}` is disabled, so launchd cannot start it; run `macron enable {0}` first")]
    JobDisabled(String),

    #[error("`{0}` is not loaded into launchd; run `macron apply` first")]
    NotLoaded(String),

    #[error("`{job}` is already running, as run {run}, and launchd does not start a job twice")]
    AlreadyRunning { job: String, run: i64 },

    #[error("launchd did not start `{job}` within {seconds} seconds; its launchd log may say why: {}", log.display())]
    DidNotStart {
        job: String,
        seconds: u64,
        log: PathBuf,
    },

    #[error("cannot read the run log {}: {reason}", path.display())]
    InvalidLog { path: PathBuf, reason: String },

    #[error("run history: {0}")]
    Database(#[from] rusqlite::Error),

    #[error(
        "the run history was written by a newer macron (schema {version}; this one reads up to \
         {supported}); upgrade macron"
    )]
    HistoryTooNew { version: usize, supported: usize },

    #[error("there is nothing at this address")]
    NotFound,

    #[error("port {0} is in use; choose another with --port, or leave it out to use any free port")]
    PortInUse(u16),

    #[error("macron is already up on port {port}; stop it before replacing the sign-in token")]
    AlreadyUp { port: u16 },

    #[error("{0}")]
    Internal(String),

    #[error("cannot encode output as JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NoHome => "no_home",
            Error::RelativePath { .. } => "relative_path",
            Error::InvalidSchedule { .. } => "invalid_schedule",
            Error::InvalidDuration { .. } => "invalid_duration",
            Error::InvalidJobName(_) => "invalid_job_name",
            Error::InvalidJob { .. } => "invalid_job",
            Error::JobExists(_) => "job_exists",
            Error::NoSuchJob(_) => "no_such_job",
            Error::InvalidConfig { .. } => "invalid_config",
            Error::Io { .. } => "io",
            Error::Launchctl { .. } => "launchctl",
            Error::Spawn { .. } => "spawn_failed",
            Error::ConfirmationRequired(_) => "confirmation_required",
            Error::NoRuns(_) => "no_runs",
            Error::NoSuchRun { .. } => "no_such_run",
            Error::JobDisabled(_) => "job_disabled",
            Error::NotLoaded(_) => "not_loaded",
            Error::AlreadyRunning { .. } => "already_running",
            Error::DidNotStart { .. } => "did_not_start",
            Error::InvalidLog { .. } => "invalid_log",
            Error::Database(_) => "database",
            Error::HistoryTooNew { .. } => "history_too_new",
            Error::NotFound => "not_found",
            Error::PortInUse(_) => "port_in_use",
            Error::AlreadyUp { .. } => "already_up",
            Error::Internal(_) | Error::Json(_) => "internal",
        }
    }

    /// An I/O failure, naming what macron was doing and to which path.
    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
    ) -> impl FnOnce(io::Error) -> Self {
        let path = path.into();
        move |source| Error::Io {
            action,
            path,
            source,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
