//! The command line. Parsing and rendering only: each command calls into the library and prints
//! what comes back, as text for people or, with `--json`, as JSON for scripts and agents.

mod jobs;
mod output;
mod runs;

use std::ffi::OsString;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};

use crate::error::{Error, Result};
use crate::exec;
use crate::notify::Notifier;
use crate::ops::Macron;
use crate::web;

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
        /// Also delete the job's run history and logs
        #[arg(long)]
        purge: bool,
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
    /// Run a job now, through launchd, as its schedule would
    Run {
        /// The job's name
        job: String,
        /// Print the run's output as it happens, and exit as the run did
        #[arg(short, long)]
        wait: bool,
    },
    /// List recent runs, newest first
    Runs(runs::RunsArgs),
    /// Print a run's output
    Logs(runs::LogsArgs),
    /// Open the web interface, serving it on 127.0.0.1 until Ctrl-C
    Up {
        /// The port to serve on [default: 6220, or any free port if that is taken]
        #[arg(long)]
        port: Option<u16>,
        /// Print the link instead of opening a browser
        #[arg(long)]
        no_open: bool,
        /// Replace the sign-in secret, signing out every browser
        #[arg(long)]
        rotate_token: bool,
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
    macron: Macron,
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
        macron: Macron::from_env()?,
    };
    match command {
        Command::Ls => jobs::list(&ctx)?,
        Command::Show { job } => jobs::show(&ctx, &job)?,
        Command::Add(args) => jobs::add(&ctx, args)?,
        Command::Edit(args) => jobs::edit(&ctx, args)?,
        Command::Rm { job, yes, purge } => jobs::remove(&ctx, &job, yes, purge)?,
        Command::Enable { job } => jobs::set_enabled(&ctx, &job, true)?,
        Command::Disable { job } => jobs::set_enabled(&ctx, &job, false)?,
        Command::Run { job, wait } => return runs::run(&ctx, &job, wait),
        Command::Runs(args) => runs::list(&ctx, args)?,
        Command::Logs(args) => runs::logs(&ctx, args)?,
        Command::Up {
            port,
            no_open,
            rotate_token,
        } => web::up(
            ctx.macron,
            web::Options {
                port,
                open: !no_open,
                rotate_token,
            },
        )?,
        Command::Apply { dry_run } => jobs::apply(&ctx, dry_run)?,
        Command::Paths => paths(&ctx)?,
        Command::Exec { job } => return exec(&ctx, &job),
    }
    Ok(ExitCode::SUCCESS)
}

fn paths(ctx: &Context) -> Result<()> {
    let paths = ctx.macron.paths();
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
    let config = ctx.macron.config()?;
    let job = config.job(name)?;
    let paths = ctx.macron.paths();
    let trigger = exec::take_trigger(paths, name);
    let outcome = exec::run(job, trigger, paths, &Notifier::from_env())?;
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
