//! The commands about runs: `run`, `runs` and `logs`.

use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use clap::Args;
use jiff::{Timestamp, Zoned};
use serde::Serialize;

use super::Context;
use super::output::{self, Paint, human_time, run_status, signal_name, took};
use crate::error::{Error, Result};
use crate::exec;
use crate::history::{Filter, History, Run, Status, Trigger};
use crate::launchd;
use crate::runlog::{self, Line, Stream};
use crate::time;

/// How long `run` waits for launchd to start the job.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a run being followed is checked for new output.
const FOLLOW_POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Args)]
#[command(after_help = "Examples:
  macron runs
  macron runs backup --limit 5
  macron runs --status failed")]
pub struct RunsArgs {
    /// Only this job's runs
    job: Option<String>,

    /// How many runs to list
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,

    /// Only runs that ended this way, or are still running
    #[arg(long, value_enum)]
    status: Option<Status>,
}

#[derive(Debug, Args)]
#[command(after_help = "Examples:
  macron logs backup
  macron logs backup --run 42
  macron logs backup --follow
  macron logs backup --raw | grep error

With --json, prints the run and its lines as one document, or with --follow, one JSON object per
line as it arrives and then the finished run as {\"run\": ...}.")]
pub struct LogsArgs {
    /// The job's name
    job: String,

    /// Which run, by the id `macron runs` shows [default: the latest]
    #[arg(long, value_name = "ID")]
    run: Option<i64>,

    /// Keep printing output as it arrives, until the run ends
    #[arg(short, long)]
    follow: bool,

    /// Print only the command's output: no times, no header and no notes from macron
    #[arg(long)]
    raw: bool,
}

/// A line of a run's log in JSON.
#[derive(Serialize)]
struct LineView<'a> {
    time: String,
    stream: Stream,
    text: &'a str,
}

impl<'a> From<&'a Line> for LineView<'a> {
    fn from(line: &'a Line) -> Self {
        let at = Timestamp::from_millisecond(line.time).unwrap_or(Timestamp::UNIX_EPOCH);
        Self {
            time: time::rfc3339(&time::local(at)),
            stream: line.stream,
            text: &line.text,
        }
    }
}

pub fn run(ctx: &Context, name: &str, wait: bool) -> Result<ExitCode> {
    let config = ctx.config()?;
    let job = config.job(name)?;
    if !job.enabled() {
        return Err(Error::JobDisabled(name.to_owned()));
    }
    let label = launchd::label(name);
    if !ctx.launchctl.is_loaded(&label)? {
        return Err(Error::NotLoaded(name.to_owned()));
    }
    let history = History::open(&ctx.paths.history_db)?;
    if let Some(running) = history.running(name)? {
        return Err(Error::AlreadyRunning {
            job: name.to_owned(),
            run: running.id,
        });
    }

    let asked_at = Timestamp::now();
    exec::request_manual_run(&ctx.paths, name)?;
    if let Err(err) = ctx.launchctl.start(&label) {
        exec::cancel_manual_run(&ctx.paths, name);
        return Err(err);
    }
    let run = match wait_for_start(&history, name, asked_at)? {
        Some(run) => run,
        None => {
            exec::cancel_manual_run(&ctx.paths, name);
            return Err(Error::DidNotStart {
                job: name.to_owned(),
                seconds: START_TIMEOUT.as_secs(),
                log: launchd::launchd_log(&ctx.paths, name),
            });
        }
    };

    if !wait {
        if ctx.json {
            output::json(&run)?;
        } else {
            println!("Started {name} as run {}.", run.id);
            println!("Follow it with `macron logs {name} --follow`.");
        }
        return Ok(ExitCode::SUCCESS);
    }
    // With --json, the finished run is the one document printed, so the output is not.
    let printer = if ctx.json {
        Printer::Silent
    } else {
        println!("Started {name} as run {}.", run.id);
        Printer::Human(Paint::stdout())
    };
    let finished = follow(&history, &run, &log_path(ctx, &run), &printer)?;
    if ctx.json {
        output::json(&finished)?;
    } else {
        println!("{}", footer(&finished));
    }
    Ok(ExitCode::from(finished.exit_status()))
}

/// Waits for the run launchd was just asked to start to show up in the history.
fn wait_for_start(history: &History, job: &str, since: Timestamp) -> Result<Option<Run>> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(run) = history.first_run_since(job, Trigger::Manual, since)? {
            return Ok(Some(run));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

pub fn list(ctx: &Context, args: RunsArgs) -> Result<()> {
    let history = History::open(&ctx.paths.history_db)?;
    let runs = history.runs(Filter {
        job: args.job.as_deref(),
        status: args.status,
        limit: args.limit,
    })?;
    if ctx.json {
        #[derive(Serialize)]
        struct Runs {
            runs: Vec<Run>,
        }
        return output::json(&Runs { runs });
    }
    if runs.is_empty() {
        let filtered = args.job.is_some() || args.status.is_some();
        println!(
            "{}",
            if filtered {
                "No runs match."
            } else {
                "No runs yet."
            }
        );
        return Ok(());
    }
    let now = Zoned::now();
    let rows: Vec<Vec<String>> = runs
        .iter()
        .map(|run| {
            let took = match run.duration_ms {
                Some(duration) => took(duration),
                None => took(now.timestamp().as_millisecond() - run.started_at.as_millisecond()),
            };
            vec![
                run.id.to_string(),
                run.job.clone(),
                human_time(&time::local(run.started_at), &now),
                took,
                run_status(run),
                run.trigger.as_str().to_owned(),
            ]
        })
        .collect();
    output::table(
        &["RUN", "JOB", "STARTED", "TOOK", "STATUS", "TRIGGER"],
        &rows,
    );
    Ok(())
}

pub fn logs(ctx: &Context, args: LogsArgs) -> Result<()> {
    let history = History::open(&ctx.paths.history_db)?;
    let run = find_run(ctx, &history, &args.job, args.run)?;
    let path = log_path(ctx, &run);

    if ctx.json && !args.follow {
        #[derive(Serialize)]
        struct Log<'a> {
            run: &'a Run,
            lines: Vec<LineView<'a>>,
        }
        let (lines, _) = runlog::read(&path, 0)?;
        return output::json(&Log {
            run: &run,
            lines: lines.iter().map(LineView::from).collect(),
        });
    }

    let printer = match (ctx.json, args.raw) {
        (true, _) => Printer::Json,
        (false, true) => Printer::Raw,
        (false, false) => Printer::Human(Paint::stdout()),
    };
    let decorated = matches!(printer, Printer::Human(_));
    if decorated {
        println!("{}", header(&run));
    }
    let (ended, printed) = if args.follow {
        let ended = follow(&history, &run, &path, &printer)?;
        (ended, None)
    } else {
        let (lines, _) = runlog::read(&path, 0)?;
        for line in &lines {
            printer.print(line)?;
        }
        (run, Some(lines.len()))
    };
    match printer {
        Printer::Json => {
            #[derive(Serialize)]
            struct Ended<'a> {
                run: &'a Run,
            }
            println!("{}", serde_json::to_string(&Ended { run: &ended })?);
        }
        Printer::Human(paint) => {
            if printed == Some(0) {
                println!("{}", paint.dim("(no output)"));
            }
            println!("{}", footer(&ended));
            if ended.status == Status::Running {
                println!("Follow it with `macron logs {} --follow`.", ended.job);
            }
        }
        Printer::Raw | Printer::Silent => {}
    }
    Ok(())
}

/// The run `id` of `job`, or its latest run.
fn find_run(ctx: &Context, history: &History, job: &str, id: Option<i64>) -> Result<Run> {
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
            ctx.config()?.job(job)?;
            Err(Error::NoRuns(job.to_owned()))
        }
    }
}

fn log_path(ctx: &Context, run: &Run) -> std::path::PathBuf {
    runlog::path(&ctx.paths, &run.job, run.id)
}

/// Prints the run's log from the start, then each new line as it arrives, until the run ends.
/// Returns the run as it ended.
fn follow(history: &History, run: &Run, path: &Path, printer: &Printer) -> Result<Run> {
    let mut offset = 0;
    loop {
        let (lines, next) = runlog::read(path, offset)?;
        offset = next;
        for line in &lines {
            printer.print(line)?;
        }
        history.mark_lost()?;
        let current = history.run(run.id)?.ok_or_else(|| Error::NoSuchRun {
            job: run.job.clone(),
            run: run.id,
        })?;
        if current.status != Status::Running {
            // Every line is written before the run is marked finished, so this read gets the rest.
            let (lines, _) = runlog::read(path, offset)?;
            for line in &lines {
                printer.print(line)?;
            }
            return Ok(current);
        }
        thread::sleep(FOLLOW_POLL);
    }
}

/// How the lines of a log are printed.
enum Printer {
    /// With the time, stderr in red and macron's notes dimmed.
    Human(Paint),
    /// The command's output alone, as it printed it.
    Raw,
    /// One JSON object per line.
    Json,
    /// Not at all.
    Silent,
}

impl Printer {
    fn print(&self, line: &Line) -> Result<()> {
        match self {
            Printer::Human(paint) => {
                let at = Timestamp::from_millisecond(line.time).unwrap_or(Timestamp::UNIX_EPOCH);
                let at = time::local(at).strftime("%H:%M:%S");
                match line.stream {
                    Stream::Out => println!("{at}  {}", line.text),
                    Stream::Err => println!("{at}  {}", paint.red(&line.text)),
                    Stream::Macron => {
                        println!("{at}  {}", paint.dim(&format!("[macron] {}", line.text)));
                    }
                }
            }
            Printer::Raw => {
                if line.stream != Stream::Macron {
                    println!("{}", line.text);
                }
            }
            Printer::Json => println!("{}", serde_json::to_string(&LineView::from(line))?),
            Printer::Silent => {}
        }
        Ok(())
    }
}

/// "Run 43 of backup, started Sat 10 Oct 23:14 on schedule."
fn header(run: &Run) -> String {
    let started = human_time(&time::local(run.started_at), &Zoned::now());
    let by = match run.trigger {
        Trigger::Schedule => "on schedule",
        Trigger::Manual => "by hand",
    };
    format!("Run {} of {}, started {started} {by}.", run.id, run.job)
}

/// "Succeeded in 2s.", "Failed with exit status 3 after 2s.", and so on.
fn footer(run: &Run) -> String {
    let took = run.duration_ms.map(took).unwrap_or_default();
    match (run.status, &run.error, run.exit_code, run.signal) {
        (Status::Running, ..) => "Still running.".to_owned(),
        (Status::Succeeded, ..) => format!("Succeeded in {took}."),
        (Status::TimedOut, ..) => format!("Stopped at its timeout after {took}."),
        (Status::Lost, error, ..) | (Status::Failed, error @ Some(_), ..) => {
            let reason = error.as_deref().unwrap_or("no reason recorded");
            let what = if run.status == Status::Lost {
                "Lost"
            } else {
                "Could not start"
            };
            format!("{what}: {reason}.")
        }
        (Status::Failed, None, Some(code), _) => {
            format!("Failed with exit status {code} after {took}.")
        }
        (Status::Failed, None, None, Some(signal)) => {
            format!("Killed by {} after {took}.", signal_name(signal))
        }
        (Status::Failed, None, None, None) => format!("Failed after {took}."),
    }
}
