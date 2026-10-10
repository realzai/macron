//! Run history: one row per run of a job, in a SQLite database in the state directory.
//!
//! Many processes use it at once. Every `macron exec` records its own run while the CLI and the web
//! UI read. WAL mode lets readers carry on while a writer writes, and a busy timeout makes writers
//! queue rather than fail.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::time::{ClockId, clock_gettime};
use nix::unistd::Pid;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;

use crate::error::{Error, Result};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The schema, one step per version: opening a database at version `n` runs every step from `n`
/// on. A released step never changes; a schema change is a new step.
const MIGRATIONS: &[&str] = &["CREATE TABLE runs (
        id INTEGER PRIMARY KEY,
        job TEXT NOT NULL,
        trigger TEXT NOT NULL,
        status TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        finished_at INTEGER,
        exit_code INTEGER,
        signal INTEGER,
        error TEXT,
        command TEXT NOT NULL,
        pid INTEGER NOT NULL,
        log_bytes INTEGER NOT NULL DEFAULT 0,
        log_truncated INTEGER NOT NULL DEFAULT 0
    ) STRICT;
    CREATE INDEX runs_by_job ON runs (job, id);"];

/// Why a run is marked lost.
const LOST: &str =
    "macron stopped recording this run before it finished: the Mac shut down, or macron was killed";

/// What started a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    /// launchd, on the job's schedule.
    Schedule,
    /// Someone, with `macron run`.
    Manual,
}

/// Where a run is, or how it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Status {
    Running,
    /// The command exited with status 0.
    Succeeded,
    /// The command exited with another status, was killed by a signal, or could not start.
    Failed,
    /// The job's timeout stopped the command.
    TimedOut,
    /// The run never finished recording: the Mac shut down, or `macron exec` was killed.
    Lost,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Schedule => "schedule",
            Trigger::Manual => "manual",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Trigger::Schedule, Trigger::Manual]
            .into_iter()
            .find(|trigger| trigger.as_str() == text)
    }
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Succeeded => "succeeded",
            Status::Failed => "failed",
            Status::TimedOut => "timed_out",
            Status::Lost => "lost",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [
            Status::Running,
            Status::Succeeded,
            Status::Failed,
            Status::TimedOut,
            Status::Lost,
        ]
        .into_iter()
        .find(|status| status.as_str() == text)
    }
}

/// One run of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Run {
    pub id: i64,
    pub job: String,
    pub trigger: Trigger,
    pub status: Status,
    #[serde(serialize_with = "crate::time::serialize")]
    pub started_at: Timestamp,
    #[serde(serialize_with = "crate::time::serialize_option")]
    pub finished_at: Option<Timestamp>,
    pub duration_ms: Option<i64>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// Why macron failed the run itself: the command could not start, or the run was lost.
    pub error: Option<String>,
    /// The command as it was when the run started.
    pub command: String,
    pub log_bytes: u64,
    pub log_truncated: bool,
    /// The `macron exec` process recording the run.
    #[serde(skip)]
    pub pid: u32,
}

impl Run {
    /// The status `macron exec` exited with for this run, following shell conventions: the
    /// command's own, 128 plus the number of the signal that killed it, 124 for a timeout, and 1
    /// when the command could not start or the run was lost.
    pub fn exit_status(&self) -> u8 {
        match (self.status, self.exit_code, self.signal) {
            (Status::TimedOut, _, _) => 124,
            (_, Some(code), _) => u8::try_from(code).unwrap_or(1),
            (_, None, Some(signal)) => u8::try_from(128 + signal).unwrap_or(1),
            _ => 1,
        }
    }

    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let text = |index: usize, value: String| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                format!("unknown value {value:?}").into(),
            )
        };
        let trigger: String = row.get("trigger")?;
        let status: String = row.get("status")?;
        let started_at: i64 = row.get("started_at")?;
        let finished_at: Option<i64> = row.get("finished_at")?;
        Ok(Self {
            id: row.get("id")?,
            job: row.get("job")?,
            trigger: Trigger::parse(&trigger).ok_or_else(|| text(2, trigger))?,
            status: Status::parse(&status).ok_or_else(|| text(3, status))?,
            started_at: timestamp(started_at),
            finished_at: finished_at.map(timestamp),
            duration_ms: finished_at.map(|finished| finished - started_at),
            exit_code: row.get("exit_code")?,
            signal: row.get("signal")?,
            error: row.get("error")?,
            command: row.get("command")?,
            log_bytes: u64::try_from(row.get::<_, i64>("log_bytes")?).unwrap_or(0),
            log_truncated: row.get("log_truncated")?,
            pid: row.get("pid")?,
        })
    }
}

/// How a run ended, for [`History::finish`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finish {
    pub status: Status,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub error: Option<String>,
    pub log_bytes: u64,
    pub log_truncated: bool,
}

/// Which runs [`History::runs`] returns, newest first.
#[derive(Debug, Clone, Copy)]
pub struct Filter<'a> {
    pub job: Option<&'a str>,
    pub status: Option<Status>,
    pub limit: usize,
}

pub struct History {
    conn: Connection,
}

impl History {
    /// Opens the database at `path`, creating it or bringing its schema up to date, and marks runs
    /// that can no longer finish as lost.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(Error::io("create", dir))?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        migrate(&mut conn)?;
        let history = Self { conn };
        history.mark_lost()?;
        Ok(history)
    }

    /// Records the start of a run and returns its id.
    pub fn start(
        &self,
        job: &str,
        trigger: Trigger,
        command: &str,
        pid: u32,
        started_at: Timestamp,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs (job, trigger, status, started_at, command, pid)
             VALUES (?1, ?2, 'running', ?3, ?4, ?5)",
            params![
                job,
                trigger.as_str(),
                started_at.as_millisecond(),
                command,
                pid
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish(&self, id: i64, finish: &Finish, finished_at: Timestamp) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status = ?2, finished_at = ?3, exit_code = ?4, signal = ?5,
                 error = ?6, log_bytes = ?7, log_truncated = ?8
             WHERE id = ?1",
            params![
                id,
                finish.status.as_str(),
                finished_at.as_millisecond(),
                finish.exit_code,
                finish.signal,
                finish.error,
                i64::try_from(finish.log_bytes).unwrap_or(i64::MAX),
                finish.log_truncated
            ],
        )?;
        Ok(())
    }

    pub fn run(&self, id: i64) -> Result<Option<Run>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM runs WHERE id = ?1", [id], Run::from_row)
            .optional()?)
    }

    pub fn runs(&self, filter: Filter) -> Result<Vec<Run>> {
        let mut statement = self.conn.prepare(
            "SELECT * FROM runs
             WHERE (?1 IS NULL OR job = ?1) AND (?2 IS NULL OR status = ?2)
             ORDER BY id DESC LIMIT ?3",
        )?;
        let limit = i64::try_from(filter.limit).unwrap_or(i64::MAX);
        let runs = statement
            .query_map(
                params![filter.job, filter.status.map(Status::as_str), limit],
                Run::from_row,
            )?
            .collect::<rusqlite::Result<_>>()?;
        Ok(runs)
    }

    /// The latest run of each job that has run, by job name.
    pub fn last_runs(&self) -> Result<BTreeMap<String, Run>> {
        let mut statement = self
            .conn
            .prepare("SELECT * FROM runs WHERE id IN (SELECT MAX(id) FROM runs GROUP BY job)")?;
        let runs = statement
            .query_map([], Run::from_row)?
            .map(|run| run.map(|run| (run.job.clone(), run)))
            .collect::<rusqlite::Result<_>>()?;
        Ok(runs)
    }

    /// The job's run in progress, if there is one.
    pub fn running(&self, job: &str) -> Result<Option<Run>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM runs WHERE job = ?1 AND status = 'running' ORDER BY id DESC LIMIT 1",
                [job],
                Run::from_row,
            )
            .optional()?)
    }

    /// The job's first run with `trigger` that started at or after `since`.
    pub fn first_run_since(
        &self,
        job: &str,
        trigger: Trigger,
        since: Timestamp,
    ) -> Result<Option<Run>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM runs WHERE job = ?1 AND trigger = ?2 AND started_at >= ?3
                 ORDER BY id LIMIT 1",
                params![job, trigger.as_str(), since.as_millisecond()],
                Run::from_row,
            )
            .optional()?)
    }

    /// Deletes the job's finished runs beyond the newest `keep`, returning the ids deleted so their
    /// logs can go too.
    pub fn prune(&self, job: &str, keep: u32) -> Result<Vec<i64>> {
        let mut statement = self.conn.prepare(
            "DELETE FROM runs
             WHERE job = ?1 AND status != 'running'
               AND id NOT IN (SELECT id FROM runs WHERE job = ?1 ORDER BY id DESC LIMIT ?2)
             RETURNING id",
        )?;
        let ids = statement
            .query_map(params![job, keep], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// Deletes a run as if it never happened, for one that will not be recorded after all.
    pub fn discard(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM runs WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Deletes every finished run of the job, returning the ids deleted so their logs can go too.
    pub fn purge(&self, job: &str) -> Result<Vec<i64>> {
        let mut statement = self
            .conn
            .prepare("DELETE FROM runs WHERE job = ?1 AND status != 'running' RETURNING id")?;
        let ids = statement
            .query_map([job], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// Marks as lost every run still recorded as running whose `macron exec` is gone, or that
    /// started before the Mac last booted: after a restart, its process id may belong to something
    /// else.
    pub fn mark_lost(&self) -> Result<()> {
        let mut statement = self
            .conn
            .prepare("SELECT id, pid, started_at FROM runs WHERE status = 'running'")?;
        let running: Vec<(i64, u32, i64)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let booted_at = booted_at();
        for (id, pid, started_at) in running {
            let before_boot = booted_at.is_some_and(|booted_at| started_at < booted_at);
            if before_boot || !is_alive(pid) {
                self.conn.execute(
                    "UPDATE runs SET status = 'lost', error = ?2 WHERE id = ?1 AND status = 'running'",
                    params![id, LOST],
                )?;
            }
        }
        Ok(())
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let latest = MIGRATIONS.len();
    let version = |conn: &Connection| -> rusqlite::Result<usize> {
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        Ok(usize::try_from(version).unwrap_or(usize::MAX))
    };
    if version(conn)? == latest {
        return Ok(());
    }
    // Another process may be migrating too; the write lock makes one wait, then find nothing to do.
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = version(&transaction)?;
    if current > latest {
        return Err(Error::HistoryTooNew {
            version: current,
            supported: latest,
        });
    }
    for step in &MIGRATIONS[current..] {
        transaction.execute_batch(step)?;
    }
    transaction.pragma_update(None, "user_version", latest as i64)?;
    transaction.commit()?;
    Ok(())
}

fn timestamp(millisecond: i64) -> Timestamp {
    Timestamp::from_millisecond(millisecond).unwrap_or(Timestamp::UNIX_EPOCH)
}

/// When the Mac last booted, in milliseconds since the epoch. macOS's monotonic clock counts from
/// boot and keeps counting while asleep, so this is now less its reading.
fn booted_at() -> Option<i64> {
    let since_boot = clock_gettime(ClockId::CLOCK_MONOTONIC).ok()?;
    let since_boot = since_boot.tv_sec() * 1_000 + since_boot.tv_nsec() / 1_000_000;
    Some(Timestamp::now().as_millisecond() - since_boot)
}

/// Whether a process with this id exists. One owned by another user counts.
fn is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use jiff::ToSpan;
    use tempfile::TempDir;

    use super::*;

    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: TempDir::new().unwrap(),
            }
        }

        fn open(&self) -> History {
            History::open(&self.dir.path().join("state/history.db")).unwrap()
        }
    }

    fn me() -> u32 {
        std::process::id()
    }

    fn succeeded() -> Finish {
        Finish {
            status: Status::Succeeded,
            exit_code: Some(0),
            signal: None,
            error: None,
            log_bytes: 12,
            log_truncated: false,
        }
    }

    /// A process id that is certainly free: a child that has exited and been waited for.
    fn dead_pid() -> u32 {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn records_a_run_from_start_to_finish() {
        let fixture = Fixture::new();
        let history = fixture.open();
        let started = Timestamp::from_second(1_800_000_000).unwrap();
        let id = history
            .start("backup", Trigger::Manual, "echo hi", me(), started)
            .unwrap();

        let run = history.run(id).unwrap().unwrap();
        assert_eq!(run.status, Status::Running);
        assert_eq!(run.trigger, Trigger::Manual);
        assert_eq!(run.finished_at, None);
        assert_eq!(history.running("backup").unwrap().unwrap().id, id);

        history
            .finish(id, &succeeded(), started + 2.seconds())
            .unwrap();
        let run = history.run(id).unwrap().unwrap();
        assert_eq!(run.status, Status::Succeeded);
        assert_eq!(run.duration_ms, Some(2_000));
        assert_eq!(run.log_bytes, 12);
        assert_eq!(run.exit_status(), 0);
        assert!(history.running("backup").unwrap().is_none());
    }

    #[test]
    fn filters_runs_newest_first() {
        let fixture = Fixture::new();
        let history = fixture.open();
        let now = Timestamp::now();
        for job in ["a", "b", "a"] {
            let id = history
                .start(job, Trigger::Schedule, "x", me(), now)
                .unwrap();
            history.finish(id, &succeeded(), now).unwrap();
        }
        let failed = history
            .start("b", Trigger::Schedule, "x", me(), now)
            .unwrap();
        history
            .finish(
                failed,
                &Finish {
                    status: Status::Failed,
                    exit_code: Some(2),
                    ..succeeded()
                },
                now,
            )
            .unwrap();

        let ids = |filter: Filter| -> Vec<i64> {
            history
                .runs(filter)
                .unwrap()
                .iter()
                .map(|run| run.id)
                .collect()
        };
        let all = Filter {
            job: None,
            status: None,
            limit: 10,
        };
        assert_eq!(ids(all), [4, 3, 2, 1]);
        assert_eq!(
            ids(Filter {
                job: Some("a"),
                ..all
            }),
            [3, 1]
        );
        assert_eq!(
            ids(Filter {
                status: Some(Status::Failed),
                ..all
            }),
            [4]
        );
        assert_eq!(ids(Filter { limit: 2, ..all }), [4, 3]);

        let last: Vec<_> = history
            .last_runs()
            .unwrap()
            .into_iter()
            .map(|(job, run)| (job, run.id))
            .collect();
        assert_eq!(last, [("a".to_owned(), 3), ("b".to_owned(), 4)]);
    }

    #[test]
    fn finds_the_first_run_since_a_time() {
        let fixture = Fixture::new();
        let history = fixture.open();
        let now = Timestamp::now();
        history
            .start("a", Trigger::Manual, "x", me(), now - 1.minute())
            .unwrap();
        history
            .start("a", Trigger::Schedule, "x", me(), now)
            .unwrap();
        assert!(
            history
                .first_run_since("a", Trigger::Manual, now)
                .unwrap()
                .is_none()
        );
        let id = history.start("a", Trigger::Manual, "x", me(), now).unwrap();
        assert_eq!(
            history
                .first_run_since("a", Trigger::Manual, now)
                .unwrap()
                .unwrap()
                .id,
            id
        );
    }

    #[test]
    fn prunes_finished_runs_beyond_the_newest_kept() {
        let fixture = Fixture::new();
        let history = fixture.open();
        let now = Timestamp::now();
        let mut ids = Vec::new();
        for _ in 0..5 {
            let id = history
                .start("a", Trigger::Schedule, "x", me(), now)
                .unwrap();
            history.finish(id, &succeeded(), now).unwrap();
            ids.push(id);
        }
        let other = history
            .start("b", Trigger::Schedule, "x", me(), now)
            .unwrap();

        assert_eq!(history.prune("a", 2).unwrap(), ids[..3]);
        let left: Vec<_> = history
            .runs(Filter {
                job: Some("a"),
                status: None,
                limit: 10,
            })
            .unwrap()
            .iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(left, [ids[4], ids[3]]);
        assert!(history.run(other).unwrap().is_some());
        assert_eq!(history.purge("a").unwrap().len(), 2);
    }

    #[test]
    fn marks_a_run_lost_when_its_process_is_gone() {
        let fixture = Fixture::new();
        let id = fixture
            .open()
            .start("a", Trigger::Schedule, "x", dead_pid(), Timestamp::now())
            .unwrap();
        let run = fixture.open().run(id).unwrap().unwrap();
        assert_eq!(run.status, Status::Lost);
        assert!(run.error.as_deref().unwrap().contains("the Mac shut down"));
        assert_eq!(run.exit_status(), 1);
    }

    #[test]
    fn marks_a_run_lost_when_it_started_before_the_last_boot() {
        let fixture = Fixture::new();
        let long_ago = Timestamp::from_second(946_684_800).unwrap();
        let id = fixture
            .open()
            .start("a", Trigger::Schedule, "x", me(), long_ago)
            .unwrap();
        assert_eq!(
            fixture.open().run(id).unwrap().unwrap().status,
            Status::Lost
        );
    }

    #[test]
    fn leaves_a_live_run_running() {
        let fixture = Fixture::new();
        let id = fixture
            .open()
            .start("a", Trigger::Schedule, "x", me(), Timestamp::now())
            .unwrap();
        assert_eq!(
            fixture.open().run(id).unwrap().unwrap().status,
            Status::Running
        );
    }

    #[test]
    fn refuses_a_database_from_a_newer_macron() {
        let fixture = Fixture::new();
        drop(fixture.open());
        let path = fixture.dir.path().join("state/history.db");
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        let err = History::open(&path).err().unwrap();
        assert_eq!(err.code(), "history_too_new");
    }

    #[test]
    fn reports_exit_statuses_as_exec_does() {
        let run = |status, exit_code, signal| Run {
            id: 1,
            job: "a".to_owned(),
            trigger: Trigger::Schedule,
            status,
            started_at: Timestamp::UNIX_EPOCH,
            finished_at: None,
            duration_ms: None,
            exit_code,
            signal,
            error: None,
            command: "x".to_owned(),
            log_bytes: 0,
            log_truncated: false,
            pid: 1,
        };
        assert_eq!(run(Status::Failed, Some(3), None).exit_status(), 3);
        assert_eq!(run(Status::Failed, None, Some(9)).exit_status(), 137);
        assert_eq!(run(Status::TimedOut, None, Some(15)).exit_status(), 124);
        assert_eq!(run(Status::Failed, None, None).exit_status(), 1);
    }
}
