//! A job: what to run, when, and how.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};

use crate::duration::Duration;
use crate::error::{Error, Result};
use crate::schedule::Schedule;

/// The shell a job's command runs in when it does not name one.
pub const DEFAULT_SHELL: &str = "/bin/zsh";

/// The working directory a job's command runs in when it does not name one.
pub const DEFAULT_WORKDIR: &str = "~";

/// How many runs of a job are kept, with their output, when it does not say.
pub const DEFAULT_KEEP_RUNS: u32 = 100;

const MAX_NAME_LEN: usize = 64;

/// A job as written in `jobs.toml`, before validation. A field left out takes its default, which
/// [`Job`] fills in; keeping it `None` here means an edit never writes a default into the file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    pub schedule: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<Notify>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_runs: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// When macron posts a macOS notification about a finished run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Notify {
    Never,
    #[default]
    Failure,
    Always,
}

impl Notify {
    pub fn as_str(self) -> &'static str {
        match self {
            Notify::Never => "never",
            Notify::Failure => "failure",
            Notify::Always => "always",
        }
    }
}

/// A validated job, with its defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    name: String,
    spec: JobSpec,
    schedule: Schedule,
    timeout: Option<Duration>,
}

impl Job {
    /// Validates `spec` as the job called `name`.
    pub fn new(name: &str, spec: JobSpec) -> Result<Self> {
        validate_name(name)?;
        let invalid = |reason: &str| Error::InvalidJob {
            job: name.to_owned(),
            reason: reason.to_owned(),
        };
        let schedule = spec.schedule.parse()?;
        if spec.command.trim().is_empty() {
            return Err(invalid("its command is empty"));
        }
        if let Some(shell) = &spec.shell
            && !Path::new(shell).is_absolute()
        {
            return Err(invalid(
                "its shell must be an absolute path, such as /bin/zsh",
            ));
        }
        if let Some(workdir) = &spec.workdir
            && !(Path::new(workdir).is_absolute() || workdir == "~" || workdir.starts_with("~/"))
        {
            return Err(invalid(
                "its workdir must be an absolute path or start with ~/",
            ));
        }
        let timeout = spec.timeout.as_deref().map(str::parse).transpose()?;
        if spec.keep_runs == Some(0) {
            return Err(invalid("keep_runs must be at least 1"));
        }
        if let Some(key) = spec.env.keys().find(|key| !is_env_name(key)) {
            return Err(Error::InvalidJob {
                job: name.to_owned(),
                reason: format!(
                    "env has {key:?}, which is not a variable name; use letters, digits and `_`, \
                     not starting with a digit"
                ),
            });
        }
        let has_nul = [&spec.command, &spec.schedule]
            .into_iter()
            .chain(&spec.shell)
            .chain(&spec.workdir)
            .chain(spec.env.values())
            .any(|text| text.contains('\0'));
        if has_nul {
            return Err(invalid("it contains a NUL character"));
        }
        Ok(Self {
            name: name.to_owned(),
            spec,
            schedule,
            timeout,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The job as written, for editing.
    pub fn spec(&self) -> &JobSpec {
        &self.spec
    }

    pub fn schedule(&self) -> &Schedule {
        &self.schedule
    }

    /// The schedule as written.
    pub fn schedule_text(&self) -> &str {
        &self.spec.schedule
    }

    pub fn command(&self) -> &str {
        &self.spec.command
    }

    pub fn description(&self) -> Option<&str> {
        self.spec.description.as_deref()
    }

    pub fn shell(&self) -> &Path {
        Path::new(self.spec.shell.as_deref().unwrap_or(DEFAULT_SHELL))
    }

    /// The working directory as written, `~` included.
    pub fn workdir(&self) -> &str {
        self.spec.workdir.as_deref().unwrap_or(DEFAULT_WORKDIR)
    }

    /// The working directory with a leading `~` replaced by `home`.
    pub fn resolved_workdir(&self, home: &Path) -> PathBuf {
        let workdir = self.workdir();
        match workdir.strip_prefix('~') {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(workdir),
        }
    }

    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.spec.env
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    pub fn notify(&self) -> Notify {
        self.spec.notify.unwrap_or_default()
    }

    pub fn keep_runs(&self) -> u32 {
        self.spec.keep_runs.unwrap_or(DEFAULT_KEEP_RUNS)
    }

    pub fn enabled(&self) -> bool {
        self.spec.enabled.unwrap_or(true)
    }
}

/// A job in JSON: every field with its default filled in, and the schedule in words.
impl Serialize for Job {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut job = serializer.serialize_struct("Job", 12)?;
        job.serialize_field("name", self.name())?;
        job.serialize_field("schedule", &self.spec.schedule)?;
        job.serialize_field("schedule_description", &self.schedule.describe())?;
        job.serialize_field("command", self.command())?;
        job.serialize_field("description", &self.description())?;
        job.serialize_field("workdir", self.workdir())?;
        job.serialize_field("shell", self.shell())?;
        job.serialize_field("env", self.env())?;
        job.serialize_field("timeout", &self.timeout())?;
        job.serialize_field("notify", &self.notify())?;
        job.serialize_field("keep_runs", &self.keep_runs())?;
        job.serialize_field("enabled", &self.enabled())?;
        job.end()
    }
}

/// Job names become part of launchd labels and file names, and the Mac's file system ignores case,
/// so they are lowercase.
pub fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && (byte == b'-' || byte == b'_'))
        });
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidJobName(name.to_owned()))
    }
}

fn is_env_name(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> JobSpec {
        JobSpec {
            schedule: "0 9 * * *".to_owned(),
            command: "echo hi".to_owned(),
            ..JobSpec::default()
        }
    }

    fn reason(spec: JobSpec) -> String {
        match Job::new("job", spec) {
            Err(Error::InvalidJob { reason, .. }) => reason,
            other => panic!("expected an invalid job, got {other:?}"),
        }
    }

    #[test]
    fn fills_in_defaults() {
        let job = Job::new("backup", spec()).unwrap();
        assert_eq!(job.shell(), Path::new("/bin/zsh"));
        assert_eq!(job.workdir(), "~");
        assert_eq!(job.notify(), Notify::Failure);
        assert_eq!(job.keep_runs(), 100);
        assert_eq!(job.timeout(), None);
        assert!(job.enabled());
    }

    #[test]
    fn accepts_names_that_are_safe_as_labels_and_file_names() {
        for name in ["backup", "a", "photo-sync", "sync_2", "0day"] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
        for name in [
            "",
            "Backup",
            "-x",
            "_x",
            "a b",
            "a.b",
            "a/b",
            &"a".repeat(65),
        ] {
            assert!(
                matches!(validate_name(name), Err(Error::InvalidJobName(_))),
                "{name}"
            );
        }
    }

    #[test]
    fn checks_the_schedule_and_timeout() {
        let bad_schedule = JobSpec {
            schedule: "0 25 * * *".to_owned(),
            ..spec()
        };
        assert!(matches!(
            Job::new("job", bad_schedule),
            Err(Error::InvalidSchedule { .. })
        ));
        let bad_timeout = JobSpec {
            timeout: Some("1mo".to_owned()),
            ..spec()
        };
        assert!(matches!(
            Job::new("job", bad_timeout),
            Err(Error::InvalidDuration { .. })
        ));
    }

    #[test]
    fn explains_what_is_wrong() {
        assert_eq!(
            reason(JobSpec {
                command: "  ".to_owned(),
                ..spec()
            }),
            "its command is empty"
        );
        assert_eq!(
            reason(JobSpec {
                shell: Some("zsh".to_owned()),
                ..spec()
            }),
            "its shell must be an absolute path, such as /bin/zsh"
        );
        assert_eq!(
            reason(JobSpec {
                workdir: Some("projects".to_owned()),
                ..spec()
            }),
            "its workdir must be an absolute path or start with ~/"
        );
        assert_eq!(
            reason(JobSpec {
                keep_runs: Some(0),
                ..spec()
            }),
            "keep_runs must be at least 1"
        );
        assert!(
            reason(JobSpec {
                env: [("1X".to_owned(), "y".to_owned())].into(),
                ..spec()
            })
            .contains("\"1X\", which is not a variable name")
        );
        assert_eq!(
            reason(JobSpec {
                command: "echo \0".to_owned(),
                ..spec()
            }),
            "it contains a NUL character"
        );
    }

    #[test]
    fn resolves_the_workdir_against_home() {
        let home = Path::new("/Users/ada");
        let job = |workdir: Option<&str>| {
            Job::new(
                "job",
                JobSpec {
                    workdir: workdir.map(str::to_owned),
                    ..spec()
                },
            )
            .unwrap()
        };
        assert_eq!(job(None).resolved_workdir(home), home);
        assert_eq!(
            job(Some("~/src")).resolved_workdir(home),
            Path::new("/Users/ada/src")
        );
        assert_eq!(job(Some("/tmp")).resolved_workdir(home), Path::new("/tmp"));
    }

    #[test]
    fn serializes_with_defaults_and_the_schedule_in_words() {
        let job = Job::new("backup", spec()).unwrap();
        assert_eq!(
            serde_json::to_value(&job).unwrap(),
            serde_json::json!({
                "name": "backup",
                "schedule": "0 9 * * *",
                "schedule_description": "at 09:00 every day",
                "command": "echo hi",
                "description": null,
                "workdir": "~",
                "shell": "/bin/zsh",
                "env": {},
                "timeout": null,
                "notify": "failure",
                "keep_runs": 100,
                "enabled": true,
            })
        );
    }
}
