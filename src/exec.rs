//! Running a job, which is what launchd does through `macron exec <job>`.
//!
//! The command runs as `<shell> -lc <command>`. A login shell loads the profile that sets up `PATH`,
//! Homebrew's included, which matters because launchd starts jobs with an almost empty
//! environment. The command gets its own process group, so a timeout stops everything it started,
//! not only the shell.
//!
//! Each run is recorded in the history, with its output in a log. If the history cannot be opened,
//! the job runs anyway, unrecorded, with its output going to launchd's log: the job matters more
//! than the record of it.

use std::fs;
use std::io::{self, BufRead, BufReader, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

use crate::error::{Error, Result};
use crate::history::{Finish, History, Status, Trigger};
use crate::job::Job;
use crate::notify::Notifier;
use crate::paths::Paths;
use crate::runlog::{self, MAX_LINE_BYTES, Stream};

/// How long a timed-out run gets to exit after SIGTERM before it gets SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(10);

/// How often a run is checked on.
const POLL: Duration = Duration::from_millis(50);

/// How long to keep reading output after the command exits. Output still in the pipes arrives at
/// once; past this, what is left is from a background process the command started, which may hold
/// the pipes open for as long as it lives.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// How long a `macron run` request stays good. launchd starts a kickstarted job at once, so an
/// older request is one that never ran.
const MANUAL_REQUEST_TTL: Duration = Duration::from_secs(60);

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

    fn finish(self) -> Finish {
        let (status, exit_code, signal) = match self {
            Outcome::Exited(0) => (Status::Succeeded, Some(0), None),
            Outcome::Exited(code) => (Status::Failed, Some(code), None),
            Outcome::Signaled(signal) => (Status::Failed, None, Some(signal)),
            Outcome::TimedOut => (Status::TimedOut, None, None),
        };
        Finish {
            status,
            exit_code,
            signal,
            error: None,
            log_bytes: 0,
            log_truncated: false,
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

/// Runs `job` to completion, recording the run and its output.
pub fn run(job: &Job, trigger: Trigger, paths: &Paths, notifier: &Notifier) -> Result<Outcome> {
    match Recorder::start(job, trigger, paths) {
        Ok(recorder) => recorder.run(job, paths, notifier),
        Err(err) => {
            eprintln!(
                "macron: cannot record this run of `{}`, so it runs unrecorded: {err}",
                job.name()
            );
            run_unrecorded(job, &paths.home)
        }
    }
}

/// Runs `job` with its output going wherever this process's goes, recording nothing.
fn run_unrecorded(job: &Job, home: &Path) -> Result<Outcome> {
    let mut child = spawn(job, home, false).map_err(|reason| Error::Spawn {
        job: job.name().to_owned(),
        reason,
    })?;
    supervise(&mut child, job.timeout(), None, |_| {}).map_err(|err| lost_track(job, &err))
}

/// Asks for the next run of `job` to be recorded as manual. `macron run` calls this just before it
/// has launchd start the job.
pub fn request_manual_run(paths: &Paths, job: &str) -> Result<()> {
    let path = manual_request(paths, job);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(Error::io("create", dir))?;
    }
    let now = Timestamp::now().as_millisecond().to_string();
    fs::write(&path, now).map_err(Error::io("write", &path))
}

/// Withdraws a request from [`request_manual_run`] that launchd did not act on.
pub fn cancel_manual_run(paths: &Paths, job: &str) {
    let _ = fs::remove_file(manual_request(paths, job));
}

/// What started this run of `job`: `macron run`, if it asked within the last minute, and
/// otherwise launchd, on schedule. Takes the request, so it counts for one run only.
pub fn take_trigger(paths: &Paths, job: &str) -> Trigger {
    let path = manual_request(paths, job);
    let requested = fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok());
    let _ = fs::remove_file(&path);
    let fresh = requested.is_some_and(|at| {
        let age = Timestamp::now().as_millisecond() - at;
        (0..MANUAL_REQUEST_TTL.as_millis() as i64).contains(&age)
    });
    if fresh {
        Trigger::Manual
    } else {
        Trigger::Schedule
    }
}

fn manual_request(paths: &Paths, job: &str) -> std::path::PathBuf {
    paths.state_dir.join("requests").join(job)
}

/// A run being recorded: its row in the history and its log.
struct Recorder {
    history: History,
    id: i64,
    log: runlog::Writer,
}

impl Recorder {
    fn start(job: &Job, trigger: Trigger, paths: &Paths) -> Result<Self> {
        let history = History::open(&paths.history_db)?;
        let id = history.start(
            job.name(),
            trigger,
            job.command(),
            std::process::id(),
            Timestamp::now(),
        )?;
        match runlog::Writer::create(&runlog::path(paths, job.name(), id)) {
            Ok(log) => Ok(Self { history, id, log }),
            Err(err) => {
                // The run happens unrecorded instead, so it must not show in the history.
                history.discard(id)?;
                Err(err)
            }
        }
    }

    fn run(mut self, job: &Job, paths: &Paths, notifier: &Notifier) -> Result<Outcome> {
        let mut child = match spawn(job, &paths.home, true) {
            Ok(child) => child,
            Err(reason) => {
                self.note(&format!("cannot start: {reason}"));
                let finish = Finish {
                    status: Status::Failed,
                    exit_code: None,
                    signal: None,
                    error: Some(reason.clone()),
                    log_bytes: 0,
                    log_truncated: false,
                };
                self.finish(job, paths, notifier, finish)?;
                return Err(Error::Spawn {
                    job: job.name().to_owned(),
                    reason,
                });
            }
        };

        let (sender, lines) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            read_lines(stdout, Stream::Out, sender.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            read_lines(stderr, Stream::Err, sender);
        }
        let outcome = supervise(&mut child, job.timeout(), Some(lines), |line| {
            self.output(line);
        })
        .map_err(|err| lost_track(job, &err))?;

        if let (Outcome::TimedOut, Some(timeout)) = (outcome, job.timeout()) {
            self.note(&format!(
                "stopped the command when it reached its timeout of {timeout}"
            ));
        }
        let finish = Finish {
            log_bytes: self.log.bytes(),
            log_truncated: self.log.truncated(),
            ..outcome.finish()
        };
        self.finish(job, paths, notifier, finish)?;
        Ok(outcome)
    }

    fn output(&mut self, (stream, text, time): Captured) {
        if let Err(err) = self.log.output(stream, &text, time) {
            eprintln!("macron: cannot write to the log of run {}: {err}", self.id);
        }
    }

    fn note(&mut self, text: &str) {
        if let Err(err) = self.log.note(text, Timestamp::now()) {
            eprintln!("macron: cannot write to the log of run {}: {err}", self.id);
        }
    }

    /// Records how the run ended, notifies as the job asks, and drops runs past the job's limit.
    fn finish(&self, job: &Job, paths: &Paths, notifier: &Notifier, finish: Finish) -> Result<()> {
        self.history.finish(self.id, &finish, Timestamp::now())?;
        if let Some(run) = self.history.run(self.id)? {
            notifier.finished(job, &run);
        }
        for id in self.history.prune(job.name(), job.keep_runs())? {
            let _ = fs::remove_file(runlog::path(paths, job.name(), id));
        }
        Ok(())
    }
}

/// Starts `job`'s command, with its output piped back when `capture` is set. On failure, says why.
fn spawn(job: &Job, home: &Path, capture: bool) -> Result<Child, String> {
    let workdir = job.resolved_workdir(home);
    if !workdir.is_dir() {
        return Err(format!(
            "its workdir {} is not a directory",
            workdir.display()
        ));
    }
    let mut command = Command::new(job.shell());
    command
        .arg("-lc")
        .arg(job.command())
        .current_dir(&workdir)
        .envs(job.env())
        .stdin(Stdio::null())
        .process_group(0);
    if capture {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    command
        .spawn()
        .map_err(|err| format!("cannot run {}: {err}", job.shell().display()))
}

fn lost_track(job: &Job, err: &io::Error) -> Error {
    Error::Spawn {
        job: job.name().to_owned(),
        reason: format!("lost track of it: {err}"),
    }
}

/// A line of output: which stream, its text, and when it was read.
type Captured = (Stream, String, Timestamp);

/// Reads `pipe` line by line on its own thread, sending each line on. Lines longer than
/// [`MAX_LINE_BYTES`] are split. The thread ends when the pipe closes.
fn read_lines(pipe: impl Read + Send + 'static, stream: Stream, lines: Sender<Captured>) {
    thread::spawn(move || {
        let mut reader = BufReader::with_capacity(MAX_LINE_BYTES, pipe);
        let mut line = Vec::new();
        loop {
            let available = match reader.fill_buf() {
                Ok([]) => break,
                Ok(available) => available,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let room = MAX_LINE_BYTES - line.len();
            let chunk = &available[..available.len().min(room)];
            let (taken, complete) = match chunk.iter().position(|&byte| byte == b'\n') {
                Some(newline) => (newline + 1, true),
                None => (chunk.len(), chunk.len() == room),
            };
            line.extend_from_slice(&chunk[..taken]);
            reader.consume(taken);
            if complete && send(&lines, stream, &mut line).is_err() {
                return;
            }
        }
        if !line.is_empty() {
            let _ = send(&lines, stream, &mut line);
        }
    });
}

fn send(lines: &Sender<Captured>, stream: Stream, line: &mut Vec<u8>) -> Result<(), ()> {
    let text = String::from_utf8_lossy(line);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    let sent = lines.send((stream, text.to_owned(), Timestamp::now()));
    line.clear();
    sent.map_err(drop)
}

/// Where a timed-out run is in being stopped.
enum Stopping {
    No,
    /// SIGTERM was sent; SIGKILL follows at this instant.
    Terminated(Instant),
    Killed,
}

/// Waits for `child` to exit, handing each line of its output to `on_line` as it arrives, and
/// stopping its process group when `timeout` passes.
fn supervise(
    child: &mut Child,
    timeout: Option<crate::duration::Duration>,
    lines: Option<Receiver<Captured>>,
    mut on_line: impl FnMut(Captured),
) -> io::Result<Outcome> {
    let deadline = timeout.map(|timeout| Instant::now() + timeout.as_std());
    let group = Pid::from_raw(child.id() as i32);
    let mut lines = lines;
    let mut stopping = Stopping::No;
    let status = loop {
        match &lines {
            Some(receiver) => match receiver.recv_timeout(POLL) {
                Ok(line) => on_line(line),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => lines = None,
            },
            None => thread::sleep(POLL),
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let now = Instant::now();
        match stopping {
            Stopping::No if deadline.is_some_and(|deadline| now >= deadline) => {
                // The group may already be gone; either way, waiting settles it.
                let _ = killpg(group, Signal::SIGTERM);
                stopping = Stopping::Terminated(now + KILL_GRACE);
            }
            Stopping::Terminated(kill_at) if now >= kill_at => {
                let _ = killpg(group, Signal::SIGKILL);
                stopping = Stopping::Killed;
            }
            _ => {}
        }
    };
    if let Some(receiver) = lines {
        let until = Instant::now() + DRAIN_GRACE;
        while let Ok(line) = receiver.recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            on_line(line);
        }
    }
    Ok(match stopping {
        Stopping::No => Outcome::from(status),
        Stopping::Terminated(_) | Stopping::Killed => Outcome::TimedOut,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use tempfile::TempDir;

    use super::*;
    use crate::history::Run;
    use crate::job::JobSpec;
    use crate::runlog::Line;

    struct Fixture {
        _dir: TempDir,
        paths: Paths,
        notifier: Notifier,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let root = dir.path().to_owned();
            fs::create_dir(root.join("home")).unwrap();
            let paths = Paths::resolve(|key| match key {
                "HOME" => Some(OsString::from(root.join("home"))),
                "MACRON_CONFIG_DIR" => Some(root.join("config").into()),
                "MACRON_STATE_DIR" => Some(root.join("state").into()),
                _ => None,
            })
            .unwrap();
            Self {
                _dir: dir,
                paths,
                notifier: Notifier::new("/usr/bin/true"),
            }
        }

        fn run(&self, job: &Job) -> Result<Outcome> {
            run(job, Trigger::Schedule, &self.paths, &self.notifier)
        }

        fn history(&self) -> History {
            History::open(&self.paths.history_db).unwrap()
        }

        fn last_run(&self, job: &str) -> Run {
            self.history().last_runs().unwrap().remove(job).unwrap()
        }

        fn log(&self, job: &str, run: i64) -> Vec<Line> {
            runlog::read(&runlog::path(&self.paths, job, run), 0)
                .unwrap()
                .0
        }

        fn texts(&self, job: &str, run: i64, stream: Stream) -> Vec<String> {
            self.log(job, run)
                .into_iter()
                .filter(|line| line.stream == stream)
                .map(|line| line.text)
                .collect()
        }
    }

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
    fn records_a_successful_run_and_its_output() {
        let fixture = Fixture::new();
        let outcome = fixture
            .run(&job("echo one; echo two >&2; printf three", |_| ()))
            .unwrap();
        assert_eq!(outcome, Outcome::Exited(0));

        let run = fixture.last_run("test");
        assert_eq!(run.status, Status::Succeeded);
        assert_eq!(run.trigger, Trigger::Schedule);
        assert_eq!(run.exit_code, Some(0));
        assert_eq!(run.command, "echo one; echo two >&2; printf three");
        assert_eq!(run.log_bytes, "onetwothree".len() as u64);
        assert!(run.finished_at.is_some());
        assert_eq!(fixture.texts("test", run.id, Stream::Out), ["one", "three"]);
        assert_eq!(fixture.texts("test", run.id, Stream::Err), ["two"]);
    }

    #[test]
    fn keeps_stdout_and_stderr_in_the_order_written() {
        let fixture = Fixture::new();
        let command = "echo a; sleep 0.2; echo b >&2; sleep 0.2; echo c";
        fixture.run(&job(command, |_| ())).unwrap();
        let run = fixture.last_run("test");
        let lines: Vec<_> = fixture
            .log("test", run.id)
            .into_iter()
            .map(|line| (line.stream, line.text))
            .collect();
        assert_eq!(
            lines,
            [
                (Stream::Out, "a".to_owned()),
                (Stream::Err, "b".to_owned()),
                (Stream::Out, "c".to_owned()),
            ]
        );
    }

    #[test]
    fn records_how_a_run_failed() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.run(&job("exit 3", |_| ())).unwrap(),
            Outcome::Exited(3)
        );
        let run = fixture.last_run("test");
        assert_eq!((run.status, run.exit_code), (Status::Failed, Some(3)));

        let outcome = fixture.run(&job("kill -TERM $$", |_| ())).unwrap();
        assert_eq!(outcome, Outcome::Signaled(15));
        assert_eq!(outcome.exit_status(), 143);
        let run = fixture.last_run("test");
        assert_eq!((run.status, run.signal), (Status::Failed, Some(15)));
    }

    #[test]
    fn runs_in_the_workdir_with_the_jobs_environment() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.paths.home.join("work")).unwrap();
        let job = job("printf %s \"$GREETING\"; pwd -P", |spec| {
            spec.workdir = Some("~/work".to_owned());
            spec.env = [("GREETING".to_owned(), "hello ".to_owned())].into();
        });
        fixture.run(&job).unwrap();
        let run = fixture.last_run("test");
        let out = fixture.texts("test", run.id, Stream::Out);
        let workdir = fs::canonicalize(fixture.paths.home.join("work")).unwrap();
        assert_eq!(out, [format!("hello {}", workdir.display())]);
    }

    #[test]
    fn a_timeout_stops_the_whole_process_group() {
        let fixture = Fixture::new();
        // The shell starts a child that would outlive it if only the shell were stopped.
        let job = job("sleep 30 & echo $!; wait", |spec| {
            spec.timeout = Some("1s".to_owned());
        });
        let started = Instant::now();
        let outcome = fixture.run(&job).unwrap();
        assert_eq!(outcome, Outcome::TimedOut);
        assert_eq!(outcome.exit_status(), 124);
        assert!(started.elapsed() < Duration::from_secs(5));

        let run = fixture.last_run("test");
        assert_eq!(run.status, Status::TimedOut);
        assert_eq!(
            fixture.texts("test", run.id, Stream::Macron),
            ["stopped the command when it reached its timeout of 1s"]
        );
        let pid: i32 = fixture.texts("test", run.id, Stream::Out)[0]
            .parse()
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        let alive = nix::sys::signal::kill(Pid::from_raw(pid), None).is_ok();
        assert!(!alive, "the background sleep should have been stopped too");
    }

    #[test]
    fn a_background_process_holding_the_output_does_not_hold_up_the_run() {
        let fixture = Fixture::new();
        let job = job("sleep 30 & echo $!", |_| ());
        let started = Instant::now();
        fixture.run(&job).unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));

        let run = fixture.last_run("test");
        assert_eq!(run.status, Status::Succeeded);
        let pid: i32 = fixture.texts("test", run.id, Stream::Out)[0]
            .parse()
            .unwrap();
        let _ = nix::sys::signal::kill(Pid::from_raw(pid), Signal::SIGKILL);
    }

    #[test]
    fn splits_lines_longer_than_the_limit() {
        let fixture = Fixture::new();
        let length = MAX_LINE_BYTES + 10;
        fixture
            .run(&job(
                &format!("head -c {length} /dev/zero | tr '\\0' x"),
                |_| (),
            ))
            .unwrap();
        let run = fixture.last_run("test");
        let lengths: Vec<_> = fixture
            .texts("test", run.id, Stream::Out)
            .iter()
            .map(String::len)
            .collect();
        assert_eq!(lengths, [MAX_LINE_BYTES, 10]);
    }

    #[test]
    fn a_run_that_cannot_start_is_recorded_as_failed() {
        let fixture = Fixture::new();
        let job = job("true", |spec| {
            spec.workdir = Some("/nonexistent/macron".to_owned())
        });
        let err = fixture.run(&job).unwrap_err();
        assert_eq!(err.code(), "spawn_failed");

        let run = fixture.last_run("test");
        assert_eq!(run.status, Status::Failed);
        assert_eq!(
            run.error.as_deref(),
            Some("its workdir /nonexistent/macron is not a directory")
        );
        assert_eq!(
            fixture.texts("test", run.id, Stream::Macron),
            ["cannot start: its workdir /nonexistent/macron is not a directory"]
        );
    }

    #[test]
    fn keeps_only_the_newest_runs_and_their_logs() {
        let fixture = Fixture::new();
        let job = job("true", |spec| spec.keep_runs = Some(2));
        for _ in 0..3 {
            fixture.run(&job).unwrap();
        }
        let ids: Vec<_> = fixture
            .history()
            .runs(crate::history::Filter {
                job: Some("test"),
                status: None,
                limit: 10,
            })
            .unwrap()
            .iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(ids, [3, 2]);
        assert!(!runlog::path(&fixture.paths, "test", 1).exists());
        assert!(runlog::path(&fixture.paths, "test", 2).exists());
    }

    #[test]
    fn a_manual_request_counts_once_and_only_while_fresh() {
        let fixture = Fixture::new();
        let paths = &fixture.paths;
        assert_eq!(take_trigger(paths, "test"), Trigger::Schedule);

        request_manual_run(paths, "test").unwrap();
        assert_eq!(take_trigger(paths, "test"), Trigger::Manual);
        assert_eq!(take_trigger(paths, "test"), Trigger::Schedule);

        request_manual_run(paths, "test").unwrap();
        let stale = Timestamp::now().as_millisecond() - 120_000;
        fs::write(manual_request(paths, "test"), stale.to_string()).unwrap();
        assert_eq!(take_trigger(paths, "test"), Trigger::Schedule);

        request_manual_run(paths, "test").unwrap();
        cancel_manual_run(paths, "test");
        assert_eq!(take_trigger(paths, "test"), Trigger::Schedule);
    }

    #[test]
    fn runs_unrecorded_when_the_history_cannot_be_opened() {
        let fixture = Fixture::new();
        fs::create_dir_all(&fixture.paths.history_db).unwrap();
        let outcome = fixture.run(&job("exit 4", |_| ())).unwrap();
        assert_eq!(outcome, Outcome::Exited(4));
    }
}
