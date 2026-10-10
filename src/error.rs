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
            Error::Json(_) => "internal",
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
