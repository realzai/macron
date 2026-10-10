//! The commands that manage jobs: `ls`, `show`, `add`, `edit`, `rm`, `enable`, `disable` and
//! `apply`.
//!
//! Every change goes the same way: edit `jobs.toml` under its lock, save it, then make launchd match
//! the saved file. A failure after the save leaves the file changed and launchd behind, which
//! `macron apply` finishes.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};

use clap::{Args, ValueEnum};
use jiff::Zoned;
use serde::Serialize;

use super::Context;
use super::output::{self, ago, human_time, run_status, tilde, took};
use crate::config::{Config, ConfigEditor};
use crate::error::{Error, Result};
use crate::history::{History, Run};
use crate::job::{Job, JobSpec, Notify};
use crate::launchd::{self, Action, Change, State};
use crate::{runlog, time};

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

/// A job in JSON, with where it stands, when it runs next and how it last ran.
#[derive(Serialize)]
struct JobView<'a> {
    #[serde(flatten)]
    job: &'a Job,
    state: State,
    next_run: Option<String>,
    last_run: Option<&'a Run>,
}

impl<'a> JobView<'a> {
    fn new(job: &'a Job, state: State, last_run: Option<&'a Run>, now: &Zoned) -> Self {
        Self {
            job,
            state,
            next_run: next_runs(job, now).next().as_ref().map(time::rfc3339),
            last_run,
        }
    }
}

/// The latest run of each job, by name.
fn last_runs(ctx: &Context) -> Result<BTreeMap<String, Run>> {
    History::open(&ctx.paths.history_db)?.last_runs()
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

/// The job's upcoming runs: none while it is disabled, and none for `@every`, which launchd counts
/// from when it loaded the job.
fn next_runs(job: &Job, now: &Zoned) -> impl Iterator<Item = Zoned> {
    let first = job
        .enabled()
        .then(|| job.schedule().next_after(now))
        .flatten();
    std::iter::successors(first, |previous| job.schedule().next_after(previous))
}

fn plan(ctx: &Context, config: &Config) -> Result<Vec<Change>> {
    let binary = launchd::current_binary()?;
    launchd::plan(config, &binary, &ctx.paths, &ctx.launchctl)
}

/// Makes launchd match `config`, returning what changed.
fn reconcile(ctx: &Context, config: &Config) -> Result<Vec<Change>> {
    let changes = plan(ctx, config)?;
    launchd::apply(&changes, &ctx.paths, &ctx.launchctl)?;
    Ok(changes)
}

pub fn list(ctx: &Context) -> Result<()> {
    let config = ctx.config()?;
    let changes = plan(ctx, &config)?;
    let last_runs = last_runs(ctx)?;
    let now = Zoned::now();
    let views: Vec<JobView> = config
        .jobs()
        .map(|job| {
            let state = launchd::state(job, &changes);
            JobView::new(job, state, last_runs.get(job.name()), &now)
        })
        .collect();
    let in_sync = changes.is_empty();
    if ctx.json {
        #[derive(Serialize)]
        struct Listing<'a> {
            jobs: Vec<JobView<'a>>,
            in_sync: bool,
        }
        return output::json(&Listing {
            jobs: views,
            in_sync,
        });
    }
    if views.is_empty() {
        println!(
            "No jobs yet. Add one with `macron add`, or write it into {}.",
            tilde(&ctx.paths.jobs_file, &ctx.paths.home)
        );
    } else {
        let rows: Vec<Vec<String>> = views
            .iter()
            .map(|view| {
                let next = next_runs(view.job, &now).next();
                vec![
                    view.job.name().to_owned(),
                    view.job.schedule_text().to_owned(),
                    next.map_or_else(|| "-".to_owned(), |next| human_time(&next, &now)),
                    last_run_cell(view.last_run, &now),
                    view.state.as_str().to_owned(),
                ]
            })
            .collect();
        output::table(
            &["NAME", "SCHEDULE", "NEXT RUN", "LAST RUN", "STATE"],
            &rows,
        );
    }
    if !in_sync {
        println!("\nlaunchd does not match jobs.toml. Run `macron apply` to fix that.");
    }
    Ok(())
}

pub fn show(ctx: &Context, name: &str) -> Result<()> {
    let config = ctx.config()?;
    let job = config.job(name)?;
    let changes = plan(ctx, &config)?;
    let state = launchd::state(job, &changes);
    let last_run = last_runs(ctx)?.remove(name);
    let now = Zoned::now();
    let plist = launchd::plist_path(&ctx.paths, name);
    let log = launchd::launchd_log(&ctx.paths, name);
    if ctx.json {
        #[derive(Serialize)]
        struct Details<'a> {
            #[serde(flatten)]
            view: JobView<'a>,
            next_runs: Vec<String>,
            label: String,
            plist: &'a std::path::Path,
            launchd_log: &'a std::path::Path,
        }
        return output::json(&Details {
            view: JobView::new(job, state, last_run.as_ref(), &now),
            next_runs: next_runs(job, &now)
                .take(NEXT_RUNS)
                .map(|time| time::rfc3339(&time))
                .collect(),
            label: launchd::label(name),
            plist: &plist,
            launchd_log: &log,
        });
    }
    let home = &ctx.paths.home;
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
            last_run.as_ref().map_or_else(
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
        ("state", state.as_str().to_owned()),
        ("label", launchd::label(name)),
        ("plist", tilde(&plist, home)),
        ("launchd log", tilde(&log, home)),
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
    let mut editor = ConfigEditor::open(&ctx.paths)?;
    editor.insert(&args.job, &spec)?;
    let config = editor.save()?;
    let changes = reconcile(ctx, &config)?;
    report_change(ctx, "Added", &config, &args.job, &changes)
}

pub fn edit(ctx: &Context, args: EditArgs) -> Result<()> {
    let mut editor = ConfigEditor::open(&ctx.paths)?;
    let current = editor.config()?.job(&args.job)?.spec().clone();
    let mut spec = current.clone();
    for field in &args.unset {
        field.unset(&mut spec);
    }
    for key in &args.unset_env {
        spec.env.remove(key);
    }
    if let Some(schedule) = args.schedule {
        spec.schedule = schedule;
    }
    if !args.command.is_empty() {
        spec.command = command_line(&args.command);
    }
    args.options.apply_to(&mut spec);
    if spec == current {
        let config = editor.config()?;
        drop(editor);
        return report_change(ctx, "Nothing to change in", &config, &args.job, &[]);
    }
    editor.update(&args.job, &spec)?;
    let config = editor.save()?;
    let changes = reconcile(ctx, &config)?;
    report_change(ctx, "Updated", &config, &args.job, &changes)
}

pub fn set_enabled(ctx: &Context, name: &str, enabled: bool) -> Result<()> {
    let mut editor = ConfigEditor::open(&ctx.paths)?;
    let mut spec = editor.config()?.job(name)?.spec().clone();
    // Enabled is the default, so enabling removes the setting rather than writing it.
    spec.enabled = (!enabled).then_some(false);
    editor.update(name, &spec)?;
    let config = editor.save()?;
    let changes = reconcile(ctx, &config)?;
    report_change(
        ctx,
        if enabled { "Enabled" } else { "Disabled" },
        &config,
        name,
        &changes,
    )
}

pub fn remove(ctx: &Context, name: &str, yes: bool, purge: bool) -> Result<()> {
    ctx.config()?.job(name)?;
    if !yes && !confirm(ctx, name, purge)? {
        println!("Kept {name}.");
        return Ok(());
    }
    let mut editor = ConfigEditor::open(&ctx.paths)?;
    editor.remove(name)?;
    let config = editor.save()?;
    let changes = reconcile(ctx, &config)?;
    let purged = if purge { purge_history(ctx, name)? } else { 0 };
    if ctx.json {
        #[derive(Serialize)]
        struct Removed<'a> {
            removed: &'a str,
            purged_runs: usize,
            changes: &'a [Change],
        }
        return output::json(&Removed {
            removed: name,
            purged_runs: purged,
            changes: &changes,
        });
    }
    println!("Removed {name}.");
    match (purge, purged) {
        (false, _) => println!("Its run history stays; `macron runs {name}` shows it."),
        (true, 1) => println!("Deleted its 1 run and its log."),
        (true, purged) => println!("Deleted its {purged} runs and their logs."),
    }
    report_other_changes(name, &changes);
    Ok(())
}

/// Deletes the job's finished runs and their logs, returning how many. A run still going keeps its
/// record and its log.
fn purge_history(ctx: &Context, name: &str) -> Result<usize> {
    let ids = History::open(&ctx.paths.history_db)?.purge(name)?;
    for id in &ids {
        let log = runlog::path(&ctx.paths, name, *id);
        if let Err(err) = fs::remove_file(&log)
            && err.kind() != io::ErrorKind::NotFound
        {
            return Err(Error::io("delete", &log)(err));
        }
    }
    // Only goes if empty, which it is unless a run is still writing to it.
    let _ = fs::remove_dir(ctx.paths.logs_dir.join(name));
    Ok(ids.len())
}

pub fn apply(ctx: &Context, dry_run: bool) -> Result<()> {
    let config = ctx.config()?;
    let changes = plan(ctx, &config)?;
    if !dry_run {
        launchd::apply(&changes, &ctx.paths, &ctx.launchctl)?;
    }
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
fn report_change(
    ctx: &Context,
    verb: &str,
    config: &Config,
    name: &str,
    changes: &[Change],
) -> Result<()> {
    let job = config.job(name)?;
    // Every change has just been applied, so the job is where its config puts it.
    let state = if job.enabled() {
        State::Active
    } else {
        State::Disabled
    };
    let last_run = last_runs(ctx)?.remove(name);
    let now = Zoned::now();
    if ctx.json {
        #[derive(Serialize)]
        struct Changed<'a> {
            job: JobView<'a>,
            changes: &'a [Change],
        }
        return output::json(&Changed {
            job: JobView::new(job, state, last_run.as_ref(), &now),
            changes,
        });
    }
    println!("{verb} {name}.");
    let mut summary = vec![("runs", job.schedule().describe())];
    if let Some(next) = next_runs(job, &now).next() {
        summary.push(("next", human_time(&next, &now)));
    }
    summary.push(("state", state.as_str().to_owned()));
    output::fields(&summary);
    report_other_changes(name, changes);
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
