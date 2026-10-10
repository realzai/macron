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

    #[error("cannot encode output as JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Error::NoHome => "no_home",
            Error::RelativePath { .. } => "relative_path",
            Error::Json(_) => "internal",
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
