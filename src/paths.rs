//! Where macron reads and writes.
//!
//! Config is what you edit (`jobs.toml`). State is what macron writes: run history and run logs.
//! Every location can be moved with an environment variable, which is also how tests stay out of
//! the real `~/Library/LaunchAgents`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub jobs_file: PathBuf,
    pub state_dir: PathBuf,
    pub history_db: PathBuf,
    pub logs_dir: PathBuf,
    /// Where launchd writes what a job's process prints before macron captures it, and errors from
    /// macron itself when it cannot start a run.
    pub launchd_logs_dir: PathBuf,
    /// Where launchd looks for per-user agents, and so where macron writes its plists.
    pub launch_agents_dir: PathBuf,
}

impl Paths {
    /// Resolves every location from the process environment.
    pub fn from_env() -> Result<Self> {
        Self::resolve(|key| std::env::var_os(key))
    }

    /// Resolves every location from `var`, a lookup into an environment.
    ///
    /// Each directory comes from, in order: its `MACRON_*` variable, used as-is; the XDG base
    /// directory with `macron` appended; the XDG default under `HOME`. An XDG variable that is not
    /// an absolute path is ignored, as the XDG spec requires. A `MACRON_*` one is an error, because
    /// someone set it on purpose.
    pub fn resolve(var: impl Fn(&str) -> Option<OsString>) -> Result<Self> {
        let home = absolute(&var, "HOME")?.ok_or(Error::NoHome)?;
        let config_dir = app_dir(
            &var,
            "MACRON_CONFIG_DIR",
            "XDG_CONFIG_HOME",
            &home.join(".config"),
        )?;
        let state_dir = app_dir(
            &var,
            "MACRON_STATE_DIR",
            "XDG_STATE_HOME",
            &home.join(".local/state"),
        )?;
        let launch_agents_dir = absolute(&var, "MACRON_LAUNCH_AGENTS_DIR")?
            .unwrap_or_else(|| home.join("Library/LaunchAgents"));

        Ok(Self {
            jobs_file: config_dir.join("jobs.toml"),
            config_dir,
            history_db: state_dir.join("history.db"),
            logs_dir: state_dir.join("logs"),
            launchd_logs_dir: state_dir.join("launchd"),
            state_dir,
            launch_agents_dir,
        })
    }
}

fn app_dir(
    var: &impl Fn(&str) -> Option<OsString>,
    own: &'static str,
    xdg: &str,
    default_base: &Path,
) -> Result<PathBuf> {
    if let Some(dir) = absolute(var, own)? {
        return Ok(dir);
    }
    let base = var(xdg)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| default_base.to_path_buf());
    Ok(base.join("macron"))
}

/// Reads `name` as an absolute path. Unset or empty is `None`; anything else that is not absolute
/// is an error.
fn absolute(
    var: &impl Fn(&str) -> Option<OsString>,
    name: &'static str,
) -> Result<Option<PathBuf>> {
    let Some(value) = var(name).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(Some(path))
    } else {
        Err(Error::RelativePath {
            var: name,
            value: path,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn resolve(vars: &[(&str, &str)]) -> Result<Paths> {
        let env: HashMap<&str, OsString> = vars
            .iter()
            .map(|(key, value)| (*key, OsString::from(value)))
            .collect();
        Paths::resolve(|key| env.get(key).cloned())
    }

    #[test]
    fn defaults_follow_xdg_under_home() {
        let paths = resolve(&[("HOME", "/Users/ada")]).unwrap();
        assert_eq!(paths.config_dir, Path::new("/Users/ada/.config/macron"));
        assert_eq!(
            paths.jobs_file,
            Path::new("/Users/ada/.config/macron/jobs.toml")
        );
        assert_eq!(paths.state_dir, Path::new("/Users/ada/.local/state/macron"));
        assert_eq!(
            paths.history_db,
            Path::new("/Users/ada/.local/state/macron/history.db")
        );
        assert_eq!(
            paths.logs_dir,
            Path::new("/Users/ada/.local/state/macron/logs")
        );
        assert_eq!(
            paths.launchd_logs_dir,
            Path::new("/Users/ada/.local/state/macron/launchd")
        );
        assert_eq!(
            paths.launch_agents_dir,
            Path::new("/Users/ada/Library/LaunchAgents")
        );
    }

    #[test]
    fn xdg_base_dirs_get_macron_appended() {
        let paths = resolve(&[
            ("HOME", "/Users/ada"),
            ("XDG_CONFIG_HOME", "/cfg"),
            ("XDG_STATE_HOME", "/state"),
        ])
        .unwrap();
        assert_eq!(paths.config_dir, Path::new("/cfg/macron"));
        assert_eq!(paths.state_dir, Path::new("/state/macron"));
    }

    #[test]
    fn relative_xdg_base_dirs_are_ignored() {
        let paths = resolve(&[("HOME", "/Users/ada"), ("XDG_CONFIG_HOME", "cfg")]).unwrap();
        assert_eq!(paths.config_dir, Path::new("/Users/ada/.config/macron"));
    }

    #[test]
    fn macron_overrides_win_and_are_used_as_is() {
        let paths = resolve(&[
            ("HOME", "/Users/ada"),
            ("XDG_CONFIG_HOME", "/cfg"),
            ("MACRON_CONFIG_DIR", "/tmp/m/config"),
            ("MACRON_STATE_DIR", "/tmp/m/state"),
            ("MACRON_LAUNCH_AGENTS_DIR", "/tmp/m/agents"),
        ])
        .unwrap();
        assert_eq!(paths.config_dir, Path::new("/tmp/m/config"));
        assert_eq!(paths.state_dir, Path::new("/tmp/m/state"));
        assert_eq!(paths.launch_agents_dir, Path::new("/tmp/m/agents"));
    }

    #[test]
    fn relative_macron_override_is_an_error() {
        let err = resolve(&[("HOME", "/Users/ada"), ("MACRON_STATE_DIR", "state")]).unwrap_err();
        assert!(matches!(
            err,
            Error::RelativePath {
                var: "MACRON_STATE_DIR",
                ..
            }
        ));
    }

    #[test]
    fn missing_home_is_an_error() {
        assert!(matches!(resolve(&[]).unwrap_err(), Error::NoHome));
    }
}
