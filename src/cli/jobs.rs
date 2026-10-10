//! The commands that manage jobs: `ls`, `show`, `add`, `edit`, `rm`, `enable`, `disable` and
//! `apply`. Each parses its arguments, calls [`Macron`](crate::ops::Macron), and prints what comes
//! back.

use std::io::{self, BufRead, IsTerminal, Write};

use clap::{Args, ValueEnum};
use jiff::Zoned;
use serde::Serialize;

use super::Context;
use super::output;
use crate::error::{Error, Result};
use crate::history::Run;
use crate::human::{ago, human_time, run_status, tilde, took};
use crate::job::{JobSpec, Notify};
use crate::launchd::{self, Action, Change};
use crate::ops::{Changed, next_runs};
use crate::time;

/// How many upcoming runs `show` lists in JSON. People get the first three.
const NEXT_RUNS: usize = 5;
const NEXT_RUNS_SHOWN: usize = 3;

#[derive(Debug, Args)]
#[command(after_help = "Examples:
  macron add backup --schedule '0 9 1 * *' -- ~/bin/backup.sh
  macron add sweep -s '@every 15m' --timeout 5m -- 'cd ~/inbox && ./sweep --all'
  macron add report -s '30 8 * * MON-FRI' -e REPORT_TO=me@example.com -- ~/bin/report")]
pub struct AddArgs {
    /// The job's name: lowercase letters, digits, `-` and `_`
    job: String,

    /// When to run: cron such as `0 9 * * 1-5`, a shorthand such as `@daily`, or `@every 15m`
    #[arg(short, long)]
    schedule: String,

    #[command(flatten)]
    options: JobOptions,

    /// Add the job without installing it
    #[arg(long)]
    disabled: bool,

    /// The command, after `--`. One argument is taken as a shell command line; several are quoted
    /// and joined into one
    #[arg(last = true, required = true, value_name = "COMMAND")]
    command: Vec<String>,
}

#[derive(Debug, Args)]
#[command(after_help = "Examples:
  macron edit backup --schedule '0 10 1 * *'
  macron edit backup --unset timeout --env MODE=full
  macron edit backup -- ~/bin/backup.sh --verbose")]
pub struct EditArgs {
    /// The job's name
    job: String,

    /// When to run: cron such as `0 9 * * 1-5`, a shorthand such as `@daily`, or `@every 15m`
    #[arg(short, long)]
    schedule: Option<String>,

    #[command(flatten)]
    options: JobOptions,

    /// Set a field back to its default; repeat for more. Applied before the options that set
    /// fields, so `--unset env --env A=1` replaces the environment
    #[arg(long, value_enum, value_name = "FIELD")]
    unset: Vec<Field>,

    /// Remove one environment variable; repeat for more
    #[arg(long, value_name = "KEY")]
    unset_env: Vec<String>,

    /// A new command, after `--`. One argument is taken as a shell command line; several are
    /// quoted and joined into one
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct JobOptions {
    /// What the job is for
    #[arg(short, long)]
    description: Option<String>,

    /// The directory to run in, absolute or under `~` [default: ~]
    #[arg(long, value_name = "DIR")]
    workdir: Option<String>,

    /// The shell to run the command with [default: /bin/zsh]
    #[arg(long, value_name = "PATH")]
    shell: Option<String>,

    /// An environment variable for the command; repeat for more
    #[arg(short, long = "env", value_name = "KEY=VALUE", value_parser = parse_env)]
    env: Vec<(String, String)>,

    /// Stop a run that takes longer than this, such as `30m` or `1h 30m`
    #[arg(long, value_name = "DURATION")]
    timeout: Option<String>,

    /// When to post a macOS notification about a finished run [default: failure]
    #[arg(long, value_enum)]
    notify: Option<Notify>,

    /// How many runs to keep, with their output [default: 100]
    #[arg(long, value_name = "N")]
    keep_runs: Option<u32>,
}

impl JobOptions {
    fn apply_to(self, spec: &mut JobSpec) {
        let set = |field: &mut Option<String>, value: Option<String>| {
            if value.is_some() {
                *field = value;
            }
        };
        set(&mut spec.description, self.description);
        set(&mut spec.workdir, self.workdir);
        set(&mut spec.shell, self.shell);
        set(&mut spec.timeout, self.timeout);
        spec.env.extend(self.env);
        spec.notify = self.notify.or(spec.notify);
        spec.keep_runs = self.keep_runs.or(spec.keep_runs);
    }
}

/// A field `edit --unset` can set back to its default.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Field {
    Description,
    Workdir,
    Shell,
    Env,
    Timeout,
    Notify,
    KeepRuns,
}

impl Field {
    fn unset(self, spec: &mut JobSpec) {
        match self {
            Field::Description => spec.description = None,
            Field::Workdir => spec.workdir = None,
            Field::Shell => spec.shell = None,
            Field::Env => spec.env.clear(),
            Field::Timeout => spec.timeout = None,
            Field::Notify => spec.notify = None,
            Field::KeepRuns => spec.keep_runs = None,
        }
    }
}

fn parse_env(text: &str) -> Result<(String, String), String> {
    let (key, value) = text
        .split_once('=')
        .ok_or_else(|| format!("`{text}` is not KEY=VALUE"))?;
    Ok((key.to_owned(), value.to_owned()))
}

/// The command line for the arguments after `--`. One argument is already a command line, such
/// as `'cd ~/src && make'`. Several are what a shell would have split, so each is quoted as
/// needed and they are joined.
fn command_line(args: &[String]) -> String {
    match args {
        [one] => one.clone(),
        many => many
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn shell_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./=:@%+,~".contains(&byte));
    if plain {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// "succeeded 5m ago", for a table cell.
fn last_run_cell(run: Option<&Run>, now: &Zoned) -> String {
    run.map_or_else(
        || "-".to_owned(),
        |run| {
            format!(
                "{} {}",
                run_status(run),
                ago(&time::local(run.started_at), now)
            )
        },
    )
}

pub fn list(ctx: &Context) -> Result<()> {
    let now = Zoned::now();
    let overview = ctx.macron.overview(&now)?;
    if ctx.json {
        return output::json(&overview);
    }
    let paths = ctx.macron.paths();
    if overview.jobs.is_empty() {
        println!(
            "No jobs yet. Add one with `macron add`, or write it into {}.",
            tilde(&paths.jobs_file, &paths.home)
        );
    } else {
        let rows: Vec<Vec<String>> = overview
            .jobs
            .iter()
            .map(|status| {
                vec![
                    status.job.name().to_owned(),
                    status.job.schedule_text().to_owned(),
                    status
                        .next_run
                        .as_ref()
                        .map_or_else(|| "-".to_owned(), |next| human_time(next, &now)),
                    last_run_cell(status.last_run.as_ref(), &now),
                    status.state.as_str().to_owned(),
                ]
            })
            .collect();
        output::table(
            &["NAME", "SCHEDULE", "NEXT RUN", "LAST RUN", "STATE"],
            &rows,
        );
    }
    if !overview.in_sync {
        println!("\nlaunchd does not match jobs.toml. Run `macron apply` to fix that.");
    }
    Ok(())
}

pub fn show(ctx: &Context, name: &str) -> Result<()> {
    let now = Zoned::now();
    let status = ctx.macron.status(name, &now)?;
    let job = &status.job;
    let paths = ctx.macron.paths();
    let plist = launchd::plist_path(paths, name);
    let log = launchd::launchd_log(paths, name);
    if ctx.json {
        #[derive(Serialize)]
        struct Details<'a> {
            #[serde(flatten)]
            status: &'a crate::ops::JobStatus,
            next_runs: Vec<String>,
            label: String,
            plist: &'a std::path::Path,
            launchd_log: &'a std::path::Path,
        }
        return output::json(&Details {
            status: &status,
            next_runs: next_runs(job, &now)
                .take(NEXT_RUNS)
                .map(|time| time::rfc3339(&time))
                .collect(),
            label: launchd::label(name),
            plist: &plist,
            launchd_log: &log,
        });
    }
    let next: Vec<String> = next_runs(job, &now)
        .take(NEXT_RUNS_SHOWN)
        .map(|time| human_time(&time, &now))
        .collect();
    let env: Vec<String> = job
        .env()
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let or_none = |value: Option<String>| value.unwrap_or_else(|| "none".to_owned());
    println!("{name}");
    output::fields(&[
        (
            "schedule",
            format!("{}  ({})", job.schedule_text(), job.schedule().describe()),
        ),
        (
            "next runs",
            if next.is_empty() {
                "-".to_owned()
            } else {
                next.join(", ")
            },
        ),
        (
            "last run",
            status.last_run.as_ref().map_or_else(
                || "none yet".to_owned(),
                |run| {
                    let started = human_time(&time::local(run.started_at), &now);
                    let took = run.duration_ms.map(took).unwrap_or_else(|| "-".to_owned());
                    format!(
                        "{}, {started}, took {took} (run {})",
                        run_status(run),
                        run.id
                    )
                },
            ),
        ),
        ("command", job.command().to_owned()),
        ("description", or_none(job.description().map(str::to_owned))),
        ("workdir", job.workdir().to_owned()),
        ("shell", job.shell().display().to_string()),
        ("env", or_none((!env.is_empty()).then(|| env.join(" ")))),
        (
            "timeout",
            or_none(job.timeout().map(|timeout| timeout.to_string())),
        ),
        ("notify", job.notify().as_str().to_owned()),
        ("keep runs", job.keep_runs().to_string()),
        ("state", status.state.as_str().to_owned()),
        ("label", launchd::label(name)),
        ("plist", tilde(&plist, &paths.home)),
        ("launchd log", tilde(&log, &paths.home)),
    ]);
    Ok(())
}

pub fn add(ctx: &Context, args: AddArgs) -> Result<()> {
    let mut spec = JobSpec {
        schedule: args.schedule,
        command: command_line(&args.command),
        ..JobSpec::default()
    };
    args.options.apply_to(&mut spec);
    if args.disabled {
        spec.enabled = Some(false);
    }
    let changed = ctx.macron.add(&args.job, &spec)?;
    report_change(ctx, "Added", &changed)
}

pub fn edit(ctx: &Context, args: EditArgs) -> Result<()> {
    let EditArgs {
        job,
        schedule,
        options,
        unset,
        unset_env,
        command,
    } = args;
    let changed = ctx.macron.edit(&job, |spec| {
        for field in &unset {
            field.unset(spec);
        }
        for key in &unset_env {
            spec.env.remove(key);
        }
        if let Some(schedule) = schedule {
            spec.schedule = schedule;
        }
        if !command.is_empty() {
            spec.command = command_line(&command);
        }
        options.apply_to(spec);
    })?;
    let verb = if changed.edited {
        "Updated"
    } else {
        "Nothing to change in"
    };
    report_change(ctx, verb, &changed)
}

pub fn set_enabled(ctx: &Context, name: &str, enabled: bool) -> Result<()> {
    let changed = ctx.macron.set_enabled(name, enabled)?;
    report_change(ctx, if enabled { "Enabled" } else { "Disabled" }, &changed)
}

pub fn remove(ctx: &Context, name: &str, yes: bool, purge: bool) -> Result<()> {
    ctx.macron.config()?.job(name)?;
    if !yes && !confirm(ctx, name, purge)? {
        println!("Kept {name}.");
        return Ok(());
    }
    let removed = ctx.macron.remove(name, purge)?;
    if ctx.json {
        return output::json(&removed);
    }
    println!("Removed {name}.");
    match (purge, removed.purged_runs) {
        (false, _) => println!("Its run history stays; `macron runs {name}` shows it."),
        (true, 1) => println!("Deleted its 1 run and its log."),
        (true, purged) => println!("Deleted its {purged} runs and their logs."),
    }
    report_other_changes(name, &removed.changes);
    Ok(())
}

pub fn apply(ctx: &Context, dry_run: bool) -> Result<()> {
    let changes = ctx.macron.apply(dry_run)?;
    if ctx.json {
        #[derive(Serialize)]
        struct Applied<'a> {
            dry_run: bool,
            changes: &'a [Change],
        }
        return output::json(&Applied {
            dry_run,
            changes: &changes,
        });
    }
    if changes.is_empty() {
        println!("Nothing to do: launchd already matches jobs.toml.");
    }
    for change in &changes {
        let verb = match (dry_run, change.action) {
            (true, action) => format!("would {}", action.as_str()),
            (false, Action::Install) => "installed".to_owned(),
            (false, Action::Update) => "updated".to_owned(),
            (false, Action::Load) => "loaded".to_owned(),
            (false, Action::Remove) => "removed".to_owned(),
        };
        println!("{verb} {}", change.job);
    }
    Ok(())
}

/// Asks on the terminal whether to remove the job. Without a terminal, or with `--json`, there is
/// no one to ask, so `--yes` is required instead.
fn confirm(ctx: &Context, name: &str, purge: bool) -> Result<bool> {
    if ctx.json || !io::stdin().is_terminal() {
        return Err(Error::ConfirmationRequired(format!("removing `{name}`")));
    }
    let history = if purge {
        "its run history and logs are deleted"
    } else {
        "its run history stays"
    };
    eprint!("Remove {name}? It is uninstalled and deleted from jobs.toml, and {history}. [y/N] ");
    io::stderr().flush().ok();
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(Error::io("read", "the answer from the terminal"))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Reports a job after a change: in JSON, the job and every change made; for people, a summary.
fn report_change(ctx: &Context, verb: &str, changed: &Changed) -> Result<()> {
    if ctx.json {
        return output::json(changed);
    }
    let status = &changed.job;
    let name = status.job.name();
    let now = Zoned::now();
    println!("{verb} {name}.");
    let mut summary = vec![("runs", status.job.schedule().describe())];
    if let Some(next) = &status.next_run {
        summary.push(("next", human_time(next, &now)));
    }
    summary.push(("state", status.state.as_str().to_owned()));
    output::fields(&summary);
    report_other_changes(name, &changed.changes);
    Ok(())
}

/// Changes to other jobs made along the way, because `jobs.toml` had been edited by hand.
fn report_other_changes(name: &str, changes: &[Change]) {
    let others: Vec<String> = changes
        .iter()
        .filter(|change| change.job != name)
        .map(|change| format!("{} {}", change.action.as_str(), change.job))
        .collect();
    if !others.is_empty() {
        println!(
            "Also applied earlier edits to jobs.toml: {}.",
            others.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn one_argument_is_a_command_line_as_it_is() {
        assert_eq!(
            command_line(&args(&["cd ~/src && make"])),
            "cd ~/src && make"
        );
    }

    #[test]
    fn several_arguments_are_quoted_where_needed_and_joined() {
        assert_eq!(
            command_line(&args(&["rsync", "-a", "~/My Photos", "it's", ""])),
            r"rsync -a '~/My Photos' 'it'\''s' ''"
        );
    }

    #[test]
    fn env_options_split_at_the_first_equals_sign() {
        assert_eq!(
            parse_env("URL=a=b"),
            Ok(("URL".to_owned(), "a=b".to_owned()))
        );
        assert!(parse_env("URL").is_err());
    }
}
