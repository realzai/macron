//! macOS notifications about finished runs.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use nix::sys::signal::Signal;

use crate::duration::Duration;
use crate::history::{Run, Status};
use crate::job::{Job, Notify};

/// Posts notifications through `osascript`.
#[derive(Debug, Clone)]
pub struct Notifier {
    program: PathBuf,
}

impl Notifier {
    /// `osascript`, or the program `MACRON_OSASCRIPT` names, which is how tests avoid posting real
    /// notifications.
    pub fn from_env() -> Self {
        let program = std::env::var_os("MACRON_OSASCRIPT")
            .filter(|program| !program.is_empty())
            .map_or_else(|| PathBuf::from("/usr/bin/osascript"), PathBuf::from);
        Self::new(program)
    }

    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// Posts the notification `job`'s setting asks for about `run`, if any. A failure to post is
    /// reported on stderr and otherwise ignored: the run is already recorded.
    pub fn finished(&self, job: &Job, run: &Run) {
        if let Some(message) = message(job, run) {
            self.post("macron", &message);
        }
    }

    /// The text goes to AppleScript as arguments, never as part of the script, so nothing in it can
    /// be run as code.
    fn post(&self, title: &str, message: &str) {
        let posted = Command::new(&self.program)
            .args([
                "-e",
                "on run argv",
                "-e",
                "display notification (item 2 of argv) with title (item 1 of argv)",
                "-e",
                "end run",
                title,
                message,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match posted {
            Ok(status) if status.success() => {}
            Ok(status) => eprintln!("macron: could not post a notification: osascript {status}"),
            Err(err) => eprintln!("macron: could not post a notification: {err}"),
        }
    }
}

/// What to tell the user about a finished run, if the job's setting asks for it.
pub fn message(job: &Job, run: &Run) -> Option<String> {
    let wanted = match job.notify() {
        Notify::Never => false,
        Notify::Failure => run.status != Status::Succeeded,
        Notify::Always => true,
    };
    if !wanted {
        return None;
    }
    let name = &run.job;
    let took = run.duration_ms.map(took).unwrap_or_default();
    let message = match run.status {
        Status::Running | Status::Lost => return None,
        Status::Succeeded => format!("{name} succeeded in {took}"),
        Status::TimedOut => match job.timeout() {
            Some(timeout) => format!("{name} was stopped when it reached its timeout of {timeout}"),
            None => format!("{name} was stopped when it reached its timeout"),
        },
        Status::Failed => match (&run.error, run.exit_code, run.signal) {
            (Some(error), _, _) => format!("{name} could not start: {error}"),
            (None, Some(code), _) => format!("{name} failed with exit status {code} after {took}"),
            (None, None, Some(signal)) => {
                let signal = Signal::try_from(signal).map_or_else(
                    |_| format!("signal {signal}"),
                    |signal| signal.as_str().to_owned(),
                );
                format!("{name} was killed by {signal} after {took}")
            }
            (None, None, None) => format!("{name} failed after {took}"),
        },
    };
    Some(message)
}

/// How long a run took, in words for a notification.
fn took(milliseconds: i64) -> String {
    match u64::try_from(milliseconds / 1_000) {
        Ok(0) | Err(_) => "less than a second".to_owned(),
        Ok(secs) => Duration::from_secs(secs).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::history::Trigger;
    use crate::job::JobSpec;

    fn job(notify: Notify) -> Job {
        let spec = JobSpec {
            schedule: "@daily".to_owned(),
            command: "true".to_owned(),
            notify: Some(notify),
            timeout: Some("30m".to_owned()),
            ..JobSpec::default()
        };
        Job::new("backup", spec).unwrap()
    }

    fn run(status: Status, exit_code: Option<i32>, signal: Option<i32>) -> Run {
        Run {
            id: 1,
            job: "backup".to_owned(),
            trigger: Trigger::Schedule,
            status,
            started_at: Timestamp::UNIX_EPOCH,
            finished_at: None,
            duration_ms: Some(62_500),
            exit_code,
            signal,
            error: None,
            command: "true".to_owned(),
            log_bytes: 0,
            log_truncated: false,
            pid: 1,
        }
    }

    #[test]
    fn follows_the_jobs_setting() {
        let succeeded = run(Status::Succeeded, Some(0), None);
        let failed = run(Status::Failed, Some(1), None);
        assert_eq!(message(&job(Notify::Never), &failed), None);
        assert_eq!(message(&job(Notify::Failure), &succeeded), None);
        assert!(message(&job(Notify::Failure), &failed).is_some());
        assert!(message(&job(Notify::Always), &succeeded).is_some());
    }

    #[test]
    fn says_how_the_run_ended() {
        let always = job(Notify::Always);
        let say = |run: Run| message(&always, &run).unwrap();
        assert_eq!(
            say(run(Status::Succeeded, Some(0), None)),
            "backup succeeded in 1m 2s"
        );
        assert_eq!(
            say(run(Status::Failed, Some(3), None)),
            "backup failed with exit status 3 after 1m 2s"
        );
        assert_eq!(
            say(run(Status::Failed, None, Some(9))),
            "backup was killed by SIGKILL after 1m 2s"
        );
        assert_eq!(
            say(run(Status::TimedOut, None, Some(15))),
            "backup was stopped when it reached its timeout of 30m"
        );
        let not_started = Run {
            error: Some("its workdir /x is not a directory".to_owned()),
            ..run(Status::Failed, None, None)
        };
        assert_eq!(
            say(not_started),
            "backup could not start: its workdir /x is not a directory"
        );
        let quick = Run {
            duration_ms: Some(300),
            ..run(Status::Succeeded, Some(0), None)
        };
        assert_eq!(say(quick), "backup succeeded in less than a second");
    }
}
