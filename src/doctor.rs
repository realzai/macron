//! `macron doctor`: what can go wrong without a sound, checked in one place.
//!
//! Each check says what it found and, when something is off, how to fix it. A problem means a job
//! will not run as configured; a warning means something deserves a look.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::config::Config;
use crate::human::tilde;
use crate::launchd;
use crate::ops::Macron;

/// How recent an error in a job's launchd log must be to report. An older one has most likely
/// been dealt with.
const RECENT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How much of a launchd log's last line to quote.
const QUOTE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Ok,
    Warning,
    Problem,
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// What was checked: `config`, `launchd`, `jobs`, `binary`, `history` or `launchd_log`. Stable,
    /// for scripts to match on.
    pub check: &'static str,
    pub severity: Severity,
    pub message: String,
    /// What to do about it, for a warning or a problem.
    pub fix: Option<String>,
}

/// Everything `doctor` found.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnosis {
    /// Whether nothing is a problem. Warnings do not count.
    pub healthy: bool,
    pub checks: Vec<Check>,
    /// What is worth knowing but cannot be checked from here.
    pub notes: Vec<String>,
}

pub fn diagnose(macron: &Macron) -> Diagnosis {
    let mut checks = Vec::new();
    let config = check_config(macron, &mut checks);
    if let Some(config) = &config {
        check_launchd(macron, config, &mut checks);
        check_jobs(macron, config, &mut checks);
    }
    check_binary(macron, &mut checks);
    check_history(macron, &mut checks);
    if let Some(config) = &config {
        check_launchd_logs(macron, config, &mut checks);
    }
    let notes = vec![format!(
        "If a job fails to read ~/Documents, ~/Desktop, ~/Downloads or another app's data, macOS is \
         blocking it. Grant Full Disk Access to {} in System Settings, under Privacy & Security. \
         macOS ties the permission to the binary, so a rebuilt or upgraded macron can need it \
         granted again.",
        macron.binary().display()
    )];
    Diagnosis {
        healthy: checks
            .iter()
            .all(|check| check.severity != Severity::Problem),
        checks,
        notes,
    }
}

fn ok(check: &'static str, message: String) -> Check {
    Check {
        check,
        severity: Severity::Ok,
        message,
        fix: None,
    }
}

fn warning(check: &'static str, message: String, fix: String) -> Check {
    Check {
        check,
        severity: Severity::Warning,
        message,
        fix: Some(fix),
    }
}

fn problem(check: &'static str, message: String, fix: String) -> Check {
    Check {
        check,
        severity: Severity::Problem,
        message,
        fix: Some(fix),
    }
}

fn check_config(macron: &Macron, checks: &mut Vec<Check>) -> Option<Config> {
    let paths = macron.paths();
    let file = tilde(&paths.jobs_file, &paths.home);
    match macron.config() {
        Ok(config) => {
            let count = config.jobs().count();
            let jobs = if count == 1 { "job" } else { "jobs" };
            checks.push(ok("config", format!("{file} has {count} {jobs}")));
            Some(config)
        }
        Err(err) => {
            checks.push(problem(
                "config",
                err.to_string(),
                format!("Fix {file}, then run `macron apply`."),
            ));
            None
        }
    }
}

fn check_launchd(macron: &Macron, config: &Config, checks: &mut Vec<Check>) {
    match macron.plan(config) {
        Ok(changes) if changes.is_empty() => {
            checks.push(ok("launchd", "launchd matches jobs.toml".to_owned()));
        }
        Ok(changes) => {
            let pending: Vec<String> = changes
                .iter()
                .map(|change| format!("{} {}", change.action.as_str(), change.job))
                .collect();
            checks.push(warning(
                "launchd",
                format!(
                    "launchd does not match jobs.toml: {} pending",
                    pending.join(", ")
                ),
                "Run `macron apply`.".to_owned(),
            ));
        }
        Err(err) => checks.push(problem(
            "launchd",
            format!("cannot ask launchd about jobs: {err}"),
            "Run `macron doctor` again; if it persists, check `launchctl` works.".to_owned(),
        )),
    }
}

/// Every enabled job's shell and working directory exist, or the job cannot start.
fn check_jobs(macron: &Macron, config: &Config, checks: &mut Vec<Check>) {
    let before = checks.len();
    for job in config.jobs().filter(|job| job.enabled()) {
        let name = job.name();
        if !is_executable(job.shell()) {
            checks.push(problem(
                "jobs",
                format!(
                    "`{name}` runs in {}, which is not an executable file",
                    job.shell().display()
                ),
                format!("Use a shell that exists: `macron edit {name} --shell /bin/zsh`."),
            ));
        }
        let workdir = job.resolved_workdir(&macron.paths().home);
        if !workdir.is_dir() {
            checks.push(problem(
                "jobs",
                format!(
                    "`{name}` runs in {}, which is not a directory",
                    workdir.display()
                ),
                format!("Create it, or choose another: `macron edit {name} --workdir <dir>`."),
            ));
        }
    }
    if checks.len() == before {
        checks.push(ok(
            "jobs",
            "every enabled job's shell and working directory exist".to_owned(),
        ));
    }
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Installed jobs run the macron that installed them. One that has since moved, as a reinstall
/// to another place does, leaves them pointing at nothing.
fn check_binary(macron: &Macron, checks: &mut Vec<Check>) {
    let current = macron.binary();
    let programs = match launchd::installed_programs(&macron.paths().launch_agents_dir) {
        Ok(programs) => programs,
        Err(err) => {
            checks.push(problem(
                "binary",
                format!("cannot read the installed jobs: {err}"),
                "Check the permissions of ~/Library/LaunchAgents.".to_owned(),
            ));
            return;
        }
    };
    let mut by_program: BTreeMap<Option<PathBuf>, Vec<String>> = BTreeMap::new();
    for (job, program) in programs {
        by_program.entry(program).or_default().push(job);
    }
    let mut found = false;
    for (program, jobs) in &by_program {
        let jobs = jobs
            .iter()
            .map(|job| format!("`{job}`"))
            .collect::<Vec<_>>()
            .join(", ");
        match program {
            Some(program) if program.as_path() == current => {}
            Some(program) if !program.exists() => {
                found = true;
                checks.push(problem(
                    "binary",
                    format!(
                        "installed jobs {jobs} run {}, which no longer exists",
                        program.display()
                    ),
                    format!(
                        "Run `macron apply` to have them run {} instead.",
                        current.display()
                    ),
                ));
            }
            Some(program) => {
                found = true;
                checks.push(warning(
                    "binary",
                    format!(
                        "installed jobs {jobs} run {}, not this macron at {}",
                        program.display(),
                        current.display()
                    ),
                    "Run `macron apply` if this is the macron they should use.".to_owned(),
                ));
            }
            None => {
                found = true;
                checks.push(problem(
                    "binary",
                    format!("the plists for {jobs} are not in the shape macron writes"),
                    "Run `macron apply` to rewrite them.".to_owned(),
                ));
            }
        }
    }
    if by_program.is_empty() {
        checks.push(ok(
            "binary",
            "no jobs are installed in launchd yet".to_owned(),
        ));
    } else if !found {
        checks.push(ok(
            "binary",
            format!("installed jobs run this macron, at {}", current.display()),
        ));
    }
}

fn check_history(macron: &Macron, checks: &mut Vec<Check>) {
    let paths = macron.paths();
    let file = tilde(&paths.history_db, &paths.home);
    match macron.history() {
        Ok(_) => checks.push(ok("history", format!("the run history at {file} opens"))),
        Err(err) => checks.push(problem(
            "history",
            err.to_string(),
            format!(
                "Runs go unrecorded until it opens. If it is damaged, move {file} aside; a new \
                 one starts empty."
            ),
        )),
    }
}

/// A job's launchd log holds only what macron itself could not handle, such as a command that
/// could not start or a run that could not be recorded, so anything recent in one is worth a look.
fn check_launchd_logs(macron: &Macron, config: &Config, checks: &mut Vec<Check>) {
    let paths = macron.paths();
    let before = checks.len();
    for job in config.jobs() {
        let log = launchd::launchd_log(paths, job.name());
        let Ok(metadata) = fs::metadata(&log) else {
            continue;
        };
        let recent = metadata
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age < RECENT);
        if metadata.len() == 0 || !recent {
            continue;
        }
        let text = fs::read_to_string(&log).unwrap_or_default();
        let last: String = text
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .chars()
            .take(QUOTE_CHARS)
            .collect();
        checks.push(warning(
            "launchd_log",
            format!("macron reported a problem running `{}`: {last}", job.name()),
            format!(
                "Read {}, and delete it once dealt with.",
                tilde(&log, &paths.home)
            ),
        ));
    }
    if checks.len() == before {
        checks.push(ok(
            "launchd_log",
            "no recent problems in the jobs' launchd logs".to_owned(),
        ));
    }
}
