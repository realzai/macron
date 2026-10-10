//! Running a job, which is what launchd does through `macron exec <job>`.
//!
//! The command runs as `<shell> -lc <command>`. A login shell loads the profile that sets up `PATH`,
//! Homebrew's included, which matters because launchd starts jobs with an almost empty
//! environment. The command gets its own process group, so a timeout stops everything it started,
//! not only the shell.

use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::error::{Error, Result};
use crate::job::Job;

/// How long a timed-out run gets to exit after SIGTERM before it gets SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(10);

/// How often a run with a timeout is checked on.
const POLL: Duration = Duration::from_millis(50);

/// The exit status for a run stopped by its timeout, as `timeout(1)` uses.
const TIMED_OUT_STATUS: u8 = 124;

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The command exited with this status.
    Exited(i32),
    /// A signal killed the command.
    Signaled(i32),
    /// The job's timeout stopped the command.
    TimedOut,
}

impl Outcome {
    /// The status `macron exec` exits with, following shell conventions: the command's own, 128
    /// plus the number of the signal that killed it, or 124 for a timeout, as `timeout(1)` uses.
    pub fn exit_status(self) -> u8 {
        match self {
            Outcome::Exited(status) => u8::try_from(status).unwrap_or(1),
            Outcome::Signaled(signal) => u8::try_from(128 + signal).unwrap_or(1),
            Outcome::TimedOut => TIMED_OUT_STATUS,
        }
    }
}

impl From<ExitStatus> for Outcome {
    fn from(status: ExitStatus) -> Self {
        match (status.code(), status.signal()) {
            (Some(code), _) => Outcome::Exited(code),
            (None, Some(signal)) => Outcome::Signaled(signal),
            (None, None) => Outcome::Exited(1),
        }
    }
}

/// Runs `job`'s command to completion, with its output going wherever this process's goes.
/// `home` is what a `~` in the job's working directory stands for.
pub fn run(job: &Job, home: &Path) -> Result<Outcome> {
    let workdir = job.resolved_workdir(home);
    let failed = |reason: String| Error::Spawn {
        job: job.name().to_owned(),
        reason,
    };
    if !workdir.is_dir() {
        return Err(failed(format!(
            "its workdir {} is not a directory",
            workdir.display()
        )));
    }
    let mut child = Command::new(job.shell())
        .arg("-lc")
        .arg(job.command())
        .current_dir(&workdir)
        .envs(job.env())
        .process_group(0)
        .spawn()
        .map_err(|err| failed(format!("cannot run {}: {err}", job.shell().display())))?;

    let waited = match job.timeout() {
        None => child.wait().map(Outcome::from),
        Some(timeout) => match wait_until(&mut child, Instant::now() + timeout.as_std()) {
            Ok(Some(status)) => Ok(Outcome::from(status)),
            Ok(None) => stop(&mut child).map(|()| Outcome::TimedOut),
            Err(err) => Err(err),
        },
    };
    waited.map_err(|err| failed(format!("lost track of it: {err}")))
}

/// Waits for `child` to exit until `deadline`. `None` means it is still running.
fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(POLL);
    }
}

/// Stops `child`'s process group: SIGTERM, then SIGKILL if it has not exited after [`KILL_GRACE`].
fn stop(child: &mut Child) -> io::Result<()> {
    let group = Pid::from_raw(child.id() as i32);
    // The group may already be gone; either way, waiting below settles it.
    let _ = killpg(group, Signal::SIGTERM);
    if wait_until(child, Instant::now() + KILL_GRACE)?.is_none() {
        let _ = killpg(group, Signal::SIGKILL);
        child.wait()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::job::JobSpec;

    fn job(command: &str, edit: impl FnOnce(&mut JobSpec)) -> Job {
        let mut spec = JobSpec {
            schedule: "@daily".to_owned(),
            command: command.to_owned(),
            shell: Some("/bin/sh".to_owned()),
            ..JobSpec::default()
        };
        edit(&mut spec);
        Job::new("test", spec).unwrap()
    }

    #[test]
    fn reports_the_commands_exit_status() {
        let home = TempDir::new().unwrap();
        assert_eq!(
            run(&job("true", |_| ()), home.path()).unwrap(),
            Outcome::Exited(0)
        );
        assert_eq!(
            run(&job("exit 3", |_| ()), home.path()).unwrap(),
            Outcome::Exited(3)
        );
    }

    #[test]
    fn reports_the_signal_that_killed_it() {
        let home = TempDir::new().unwrap();
        let outcome = run(&job("kill -TERM $$", |_| ()), home.path()).unwrap();
        assert_eq!(outcome, Outcome::Signaled(15));
        assert_eq!(outcome.exit_status(), 143);
    }

    #[test]
    fn runs_in_the_workdir_with_the_jobs_environment() {
        let home = TempDir::new().unwrap();
        fs::create_dir(home.path().join("work")).unwrap();
        let job = job("printf %s \"$GREETING\" > out.txt", |spec| {
            spec.workdir = Some("~/work".to_owned());
            spec.env = [("GREETING".to_owned(), "hello".to_owned())].into();
        });
        run(&job, home.path()).unwrap();
        let out = fs::read_to_string(home.path().join("work/out.txt")).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn a_timeout_stops_the_whole_process_group() {
        let home = TempDir::new().unwrap();
        // The shell starts a child that would outlive it if only the shell were stopped.
        let job = job("sleep 30 & echo $! > pid; wait", |spec| {
            spec.timeout = Some("1s".to_owned());
        });
        let started = Instant::now();
        let outcome = run(&job, home.path()).unwrap();
        assert_eq!(outcome, Outcome::TimedOut);
        assert_eq!(outcome.exit_status(), 124);
        assert!(started.elapsed() < Duration::from_secs(5));

        let pid: i32 = fs::read_to_string(home.path().join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        let alive = nix::sys::signal::kill(Pid::from_raw(pid), None).is_ok();
        assert!(!alive, "the background sleep should have been stopped too");
    }

    #[test]
    fn a_missing_workdir_is_reported_before_starting() {
        let home = TempDir::new().unwrap();
        let job = job("true", |spec| {
            spec.workdir = Some("/nonexistent/macron".to_owned())
        });
        let err = run(&job, home.path()).unwrap_err();
        assert_eq!(err.code(), "spawn_failed");
        assert!(
            err.to_string()
                .contains("/nonexistent/macron is not a directory")
        );
    }
}
