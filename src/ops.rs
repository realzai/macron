//! What macron does. The CLI and the web UI both work through [`Macron`], so every operation has
//! one implementation: a job changed in the browser is changed exactly as the CLI would change it.
//!
//! Every change to jobs goes the same way: edit `jobs.toml` under its lock, save it, then make
//! launchd match the saved file. A failure after the save leaves the file changed and launchd
//! behind, which [`Macron::apply`] finishes.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use jiff::{Timestamp, Zoned};
use serde::Serialize;

use crate::config::{Config, ConfigEditor};
use crate::error::{Error, Result};
use crate::exec;
use crate::history::{Filter, History, Run, Status, Trigger};
use crate::job::{Job, JobSpec};
use crate::launchd::{self, Change, Launchctl, State};
use crate::paths::Paths;
use crate::runlog::{self, Line};

/// How long [`Macron::start_run`] waits for launchd to start the job.
const START_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Macron {
    paths: Paths,
    launchctl: Launchctl,
    binary: PathBuf,
}

/// A job, where it stands with launchd, when it runs next and how it last ran.
#[derive(Debug, Clone, Serialize)]
pub struct JobStatus {
    #[serde(flatten)]
    pub job: Job,
    pub state: State,
    #[serde(serialize_with = "crate::time::serialize_zoned_option")]
    pub next_run: Option<Zoned>,
    pub last_run: Option<Run>,
}

/// Every job, in name order, and whether launchd matches `jobs.toml`.
#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub jobs: Vec<JobStatus>,
    pub in_sync: bool,
}

/// A job after a change, and what the change did to launchd.
#[derive(Debug, Clone, Serialize)]
pub struct Changed {
    pub job: JobStatus,
    pub changes: Vec<Change>,
    /// Whether `jobs.toml` changed. An edit that leaves the job as it was writes nothing.
    #[serde(skip)]
    pub edited: bool,
}

/// What removing a job did.
#[derive(Debug, Clone, Serialize)]
pub struct Removed {
    pub removed: String,
    pub purged_runs: usize,
    pub changes: Vec<Change>,
}

impl Macron {
    /// macron as the environment sets it up: its paths, the real `launchctl` (or
    /// `MACRON_LAUNCHCTL`'s), and this binary as the one launchd runs.
    pub fn from_env() -> Result<Self> {
        Ok(Self::new(
            Paths::from_env()?,
            Launchctl::from_env(),
            launchd::current_binary()?,
        ))
    }

    pub fn new(paths: Paths, launchctl: Launchctl, binary: PathBuf) -> Self {
        Self {
            paths,
            launchctl,
            binary,
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn config(&self) -> Result<Config> {
        Config::load(&self.paths.jobs_file)
    }

    pub fn history(&self) -> Result<History> {
        History::open(&self.paths.history_db)
    }

    /// The changes that would make launchd match `config`.
    pub fn plan(&self, config: &Config) -> Result<Vec<Change>> {
        launchd::plan(config, &self.binary, &self.paths, &self.launchctl)
    }

    /// Makes launchd match `jobs.toml`, or with `dry_run`, only says what that would change.
    pub fn apply(&self, dry_run: bool) -> Result<Vec<Change>> {
        let config = self.config()?;
        if dry_run {
            self.plan(&config)
        } else {
            self.reconcile(&config)
        }
    }

    fn reconcile(&self, config: &Config) -> Result<Vec<Change>> {
        let changes = self.plan(config)?;
        launchd::apply(&changes, &self.paths, &self.launchctl)?;
        Ok(changes)
    }

    /// Every job with its state, next run and last run.
    pub fn overview(&self, now: &Zoned) -> Result<Overview> {
        let config = self.config()?;
        let changes = self.plan(&config)?;
        let mut last_runs = self.history()?.last_runs()?;
        let jobs = config
            .jobs()
            .map(|job| {
                let state = launchd::state(job, &changes);
                status(job, state, last_runs.remove(job.name()), now)
            })
            .collect();
        Ok(Overview {
            jobs,
            in_sync: changes.is_empty(),
        })
    }

    /// One job with its state, next run and last run.
    pub fn status(&self, name: &str, now: &Zoned) -> Result<JobStatus> {
        let config = self.config()?;
        let job = config.job(name)?;
        let state = launchd::state(job, &self.plan(&config)?);
        let last_run = self.history()?.last_runs()?.remove(name);
        Ok(status(job, state, last_run, now))
    }

    /// Adds a job and installs it, unless it is disabled.
    pub fn add(&self, name: &str, spec: &JobSpec) -> Result<Changed> {
        let mut editor = ConfigEditor::open(&self.paths)?;
        editor.insert(name, spec)?;
        let config = editor.save()?;
        self.changed(&config, name, true)
    }

    /// Changes a job with `change` and reinstalls it. A change that leaves the job as it was
    /// writes nothing and touches nothing.
    pub fn edit(&self, name: &str, change: impl FnOnce(&mut JobSpec)) -> Result<Changed> {
        let mut editor = ConfigEditor::open(&self.paths)?;
        let config = editor.config()?;
        let current = config.job(name)?.spec();
        let mut spec = current.clone();
        change(&mut spec);
        if &spec == current {
            drop(editor);
            let job = config.job(name)?;
            let last_run = self.history()?.last_runs()?.remove(name);
            return Ok(Changed {
                job: status(job, settled(job), last_run, &Zoned::now()),
                changes: Vec::new(),
                edited: false,
            });
        }
        editor.update(name, &spec)?;
        let config = editor.save()?;
        self.changed(&config, name, true)
    }

    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<Changed> {
        // Enabled is the default, so enabling removes the setting rather than writing it.
        self.edit(name, |spec| spec.enabled = (!enabled).then_some(false))
    }

    /// Uninstalls a job and removes it from `jobs.toml`. With `purge`, its finished runs and their
    /// logs go too; otherwise its history stays.
    pub fn remove(&self, name: &str, purge: bool) -> Result<Removed> {
        let mut editor = ConfigEditor::open(&self.paths)?;
        editor.remove(name)?;
        let config = editor.save()?;
        let changes = self.reconcile(&config)?;
        let purged_runs = if purge { self.purge(name)? } else { 0 };
        Ok(Removed {
            removed: name.to_owned(),
            purged_runs,
            changes,
        })
    }

    /// Deletes the job's finished runs and their logs, returning how many. A run still going keeps
    /// its record and its log.
    fn purge(&self, name: &str) -> Result<usize> {
        let ids = self.history()?.purge(name)?;
        for id in &ids {
            let log = runlog::path(&self.paths, name, *id);
            if let Err(err) = fs::remove_file(&log)
                && err.kind() != io::ErrorKind::NotFound
            {
                return Err(Error::io("delete", &log)(err));
            }
        }
        // Only goes if empty, which it is unless a run is still writing to it.
        let _ = fs::remove_dir(self.paths.logs_dir.join(name));
        Ok(ids.len())
    }

    /// Has launchd start the job now, as its schedule would, and returns the run once it has
    /// started. Refuses a job that is disabled, not loaded, or already running, because launchd
    /// would not start it.
    pub fn start_run(&self, name: &str) -> Result<Run> {
        let config = self.config()?;
        let job = config.job(name)?;
        if !job.enabled() {
            return Err(Error::JobDisabled(name.to_owned()));
        }
        let label = launchd::label(name);
        if !self.launchctl.is_loaded(&label)? {
            return Err(Error::NotLoaded(name.to_owned()));
        }
        let history = self.history()?;
        if let Some(running) = history.running(name)? {
            return Err(Error::AlreadyRunning {
                job: name.to_owned(),
                run: running.id,
            });
        }

        let asked_at = Timestamp::now();
        exec::request_manual_run(&self.paths, name)?;
        if let Err(err) = self.launchctl.start(&label) {
            exec::cancel_manual_run(&self.paths, name);
            return Err(err);
        }
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Some(run) = history.first_run_since(name, Trigger::Manual, asked_at)? {
                return Ok(run);
            }
            if Instant::now() >= deadline {
                exec::cancel_manual_run(&self.paths, name);
                return Err(Error::DidNotStart {
                    job: name.to_owned(),
                    seconds: START_TIMEOUT.as_secs(),
                    log: launchd::launchd_log(&self.paths, name),
                });
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Run `id` of `job`, or with no id, its latest run.
    pub fn find_run(&self, job: &str, id: Option<i64>) -> Result<Run> {
        let history = self.history()?;
        if let Some(id) = id {
            return history
                .run(id)?
                .filter(|run| run.job == job)
                .ok_or_else(|| Error::NoSuchRun {
                    job: job.to_owned(),
                    run: id,
                });
        }
        let filter = Filter {
            job: Some(job),
            status: None,
            limit: 1,
        };
        match history.runs(filter)?.into_iter().next() {
            Some(run) => Ok(run),
            None => {
                // A job that never ran and one that does not exist call for different errors.
                self.config()?.job(job)?;
                Err(Error::NoRuns(job.to_owned()))
            }
        }
    }

    /// Reads `run`'s log as it grows.
    pub fn follow(&self, run: Run) -> Follower {
        Follower {
            path: runlog::path(&self.paths, &run.job, run.id),
            run,
            offset: 0,
        }
    }

    fn changed(&self, config: &Config, name: &str, edited: bool) -> Result<Changed> {
        let changes = self.reconcile(config)?;
        let job = config.job(name)?;
        let last_run = self.history()?.last_runs()?.remove(name);
        Ok(Changed {
            job: status(job, settled(job), last_run, &Zoned::now()),
            changes,
            edited,
        })
    }
}

/// Reads a run's log as it grows, until the run ends.
pub struct Follower {
    run: Run,
    path: PathBuf,
    offset: u64,
}

impl Follower {
    /// The lines written since the last call, and the run once it has ended.
    ///
    /// `exec` writes every line before it marks the run ended, so the call that returns the ended
    /// run has returned every line.
    pub fn poll(&mut self, history: &History) -> Result<(Vec<Line>, Option<Run>)> {
        let (mut lines, offset) = runlog::read(&self.path, self.offset)?;
        self.offset = offset;
        history.mark_lost()?;
        let current = history.run(self.run.id)?.ok_or_else(|| Error::NoSuchRun {
            job: self.run.job.clone(),
            run: self.run.id,
        })?;
        if current.status == Status::Running {
            return Ok((lines, None));
        }
        let (rest, offset) = runlog::read(&self.path, self.offset)?;
        self.offset = offset;
        lines.extend(rest);
        Ok((lines, Some(current)))
    }
}

/// The job's upcoming runs: none while it is disabled, and none for `@every`, which launchd counts
/// from when it loaded the job.
pub fn next_runs(job: &Job, now: &Zoned) -> impl Iterator<Item = Zoned> {
    let first = job
        .enabled()
        .then(|| job.schedule().next_after(now))
        .flatten();
    std::iter::successors(first, |previous| job.schedule().next_after(previous))
}

fn status(job: &Job, state: State, last_run: Option<Run>, now: &Zoned) -> JobStatus {
    JobStatus {
        next_run: next_runs(job, now).next(),
        job: job.clone(),
        state,
        last_run,
    }
}

/// The state of a job right after its changes were applied: where its config puts it.
fn settled(job: &Job) -> State {
    if job.enabled() {
        State::Active
    } else {
        State::Disabled
    }
}
