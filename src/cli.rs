//! The command line. Parsing and rendering only: each command calls into the library and prints
//! what comes back, as text for people or, with `--json`, as JSON for scripts and agents.

use std::ffi::OsString;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::paths::Paths;

/// Exit status for a command line that does not parse. It is clap's own, so it is the same with or
/// without `--json`.
const USAGE_EXIT: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "macron",
    version,
    about = "Schedule, run and watch local jobs on macOS"
)]
struct Cli {
    /// Print JSON instead of text. Errors go to stderr as JSON too.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show where macron reads jobs from and writes run history and logs to
    Paths,
}

pub fn run() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) => return usage_error(err, &args),
    };
    match execute(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report(&err, cli.json);
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: &Cli) -> Result<()> {
    match cli.command {
        Command::Paths => paths(cli.json),
    }
}

fn paths(json: bool) -> Result<()> {
    let paths = Paths::from_env()?;
    if json {
        return print_json(&paths);
    }
    for (label, path) in [
        ("config dir", &paths.config_dir),
        ("jobs file", &paths.jobs_file),
        ("state dir", &paths.state_dir),
        ("history db", &paths.history_db),
        ("logs dir", &paths.logs_dir),
        ("launch agents", &paths.launch_agents_dir),
    ] {
        println!("{label:<14} {}", path.display());
    }
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
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
