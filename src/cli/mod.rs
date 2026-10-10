//! The command line. Parsing and rendering only: each command calls into the library and prints
//! what comes back, as text for people or, with `--json`, as JSON for scripts and agents.

mod jobs;
mod output;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::exec;
use crate::launchd::Launchctl;
use crate::notify::Notifier;
use crate::paths::Paths;

/// Exit status for a command line that does not parse. It is clap's own, so it is the same with or
/// without `--json`.
const USAGE_EXIT: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "macron",
    version,
    about = "Schedule, run and watch local jobs on macOS",
    long_about = "Schedule, run and watch local jobs on macOS.\n\n\
        Jobs live in jobs.toml (see `macron paths`). macron installs each enabled job into \
        launchd, which runs it on schedule, including runs missed while the Mac was asleep."
)]
struct Cli {
    /// Print JSON instead of text. Errors go to stderr as JSON too
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List jobs with their schedule, next run and state
    #[command(visible_alias = "list")]
    Ls,
    /// Show one job in full
    Show {
        /// The job's name
        job: String,
    },
    /// Add a job and install it
    Add(jobs::AddArgs),
    /// Change a job and reinstall it
    Edit(jobs::EditArgs),
    /// Remove a job and uninstall it
    Rm {
        /// The job's name
        job: String,
        /// Remove without asking. Required when there is no terminal to ask on
        #[arg(short, long)]
        yes: bool,
    },
    /// Install a disabled job again
    Enable {
        /// The job's name
        job: String,
    },
    /// Uninstall a job, keeping it in jobs.toml
    Disable {
        /// The job's name
        job: String,
    },
    /// Make launchd match jobs.toml, after editing it by hand
    Apply {
        /// Show what would change without changing it
        #[arg(long)]
        dry_run: bool,
    },
    /// Show where macron reads jobs from and writes run history and logs to
    Paths,
    /// Run a job now, the way launchd does. This is what installed jobs run
    #[command(hide = true)]
    Exec {
        /// The job's name
        job: String,
    },
}

/// What every command needs.
struct Context {
    json: bool,
    paths: Paths,
    launchctl: Launchctl,
}

impl Context {
    fn config(&self) -> Result<Config> {
        Config::load(&self.paths.jobs_file)
    }
}

pub fn run() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) => return usage_error(err, &args),
    };
    match execute(cli.command, cli.json) {
        Ok(code) => code,
        Err(err) => {
            report(&err, cli.json);
            ExitCode::FAILURE
        }
    }
}

fn execute(command: Command, json: bool) -> Result<ExitCode> {
    let ctx = Context {
        json,
        paths: Paths::from_env()?,
        launchctl: Launchctl::from_env(),
    };
    match command {
        Command::Ls => jobs::list(&ctx)?,
        Command::Show { job } => jobs::show(&ctx, &job)?,
        Command::Add(args) => jobs::add(&ctx, args)?,
        Command::Edit(args) => jobs::edit(&ctx, args)?,
        Command::Rm { job, yes } => jobs::remove(&ctx, &job, yes)?,
        Command::Enable { job } => jobs::set_enabled(&ctx, &job, true)?,
        Command::Disable { job } => jobs::set_enabled(&ctx, &job, false)?,
        Command::Apply { dry_run } => jobs::apply(&ctx, dry_run)?,
        Command::Paths => paths(&ctx)?,
        Command::Exec { job } => return exec(&ctx, &job),
    }
    Ok(ExitCode::SUCCESS)
}

fn paths(ctx: &Context) -> Result<()> {
    let paths = &ctx.paths;
    if ctx.json {
        return output::json(paths);
    }
    for (label, path) in [
        ("config dir", &paths.config_dir),
        ("jobs file", &paths.jobs_file),
        ("state dir", &paths.state_dir),
        ("history db", &paths.history_db),
        ("logs dir", &paths.logs_dir),
        ("launchd logs", &paths.launchd_logs_dir),
        ("launch agents", &paths.launch_agents_dir),
    ] {
        println!("{label:<14} {}", path.display());
    }
    Ok(())
}

/// Runs the job, recording it, and exits as it did.
fn exec(ctx: &Context, name: &str) -> Result<ExitCode> {
    let config = ctx.config()?;
    let job = config.job(name)?;
    let trigger = exec::take_trigger(&ctx.paths, name);
    let outcome = exec::run(job, trigger, &ctx.paths, &Notifier::from_env())?;
    if let (exec::Outcome::TimedOut, Some(timeout)) = (outcome, job.timeout()) {
        eprintln!("macron: stopped `{name}` when it reached its timeout of {timeout}");
    }
    Ok(ExitCode::from(outcome.exit_status()))
}

fn report(err: &Error, json: bool) {
    if json {
        eprintln!("{}", error_json(err.code(), &err.to_string()));
    } else {
        eprintln!("macron: {err}");
    }
}

/// A command line that did not parse. `--help` and `--version` arrive here too and are printed as
/// clap prints them. Otherwise, with `--json` anywhere in the arguments, the error is printed as
/// JSON: the flag cannot have been parsed, so it is looked for directly.
fn usage_error(err: clap::Error, args: &[OsString]) -> ExitCode {
    let wants_json = args.iter().skip(1).any(|arg| arg == "--json");
    if !wants_json
        || matches!(
            err.kind(),
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
        )
    {
        err.exit()
    }
    let message = if err.kind() == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand {
        "a command is required; see `macron --help`".to_owned()
    } else {
        let rendered = err.render().to_string();
        let first_line = rendered.lines().next().unwrap_or_default();
        first_line.trim_start_matches("error: ").to_owned()
    };
    eprintln!("{}", error_json("usage", &message));
    ExitCode::from(USAGE_EXIT)
}

fn error_json(code: &str, message: &str) -> serde_json::Value {
    serde_json::json!({ "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_line_definition_is_consistent() {
        Cli::command().debug_assert();
    }
}
