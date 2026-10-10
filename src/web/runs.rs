//! The pages for runs: one run's output, arriving live while it runs, and recent runs across every
//! job.

use std::borrow::Cow;
use std::convert::Infallible;
use std::thread;
use std::time::Duration;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Html;
use axum::response::sse::{Event, KeepAlive, Sse};
use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use super::pages::{RunRow, RunsTable, status_class};
use super::{AppState, PageError};
use crate::history::{Filter, Run, Status};
use crate::human::{human_time, run_status, summary, took};
use crate::ops::Macron;
use crate::runlog::{self, Line, Stream as LogStream};
use crate::time;

/// How many runs the runs page lists.
const RUNS_PAGE_RUNS: usize = 100;

/// How often a live run's log is checked for new lines.
const POLL: Duration = Duration::from_millis(250);

/// A line of a run's output, as the page shows it.
#[derive(Serialize)]
struct LineView {
    time: String,
    stream: &'static str,
    text: String,
}

impl From<&Line> for LineView {
    fn from(line: &Line) -> Self {
        let at = Timestamp::from_millisecond(line.time).unwrap_or(Timestamp::UNIX_EPOCH);
        Self {
            time: time::local(at).strftime("%H:%M:%S").to_string(),
            stream: match line.stream {
                LogStream::Out => "out",
                LogStream::Err => "err",
                LogStream::Macron => "macron",
            },
            text: strip_ansi(&line.text).into_owned(),
        }
    }
}

#[derive(Template)]
#[template(path = "run.html")]
struct RunPage {
    section: &'static str,
    job: String,
    id: i64,
    status: String,
    class: &'static str,
    started: String,
    took: String,
    trigger: &'static str,
    summary: String,
    command: String,
    lines: Vec<LineView>,
    running: bool,
    /// Where the page's script picks up the output, after the lines already on the page.
    events: String,
}

pub(super) async fn run(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
) -> Result<Html<String>, PageError> {
    let (run, lines, offset) = state
        .run(move |macron| {
            let run = macron.find_run(&name, Some(id))?;
            let (lines, offset) = runlog::read(&runlog::path(macron.paths(), &run.job, run.id), 0)?;
            Ok((run, lines, offset))
        })
        .await?;
    let now = Zoned::now();
    let running = run.status == Status::Running;
    let took = match run.duration_ms {
        Some(duration) => took(duration),
        None => took(now.timestamp().as_millisecond() - run.started_at.as_millisecond()),
    };
    let page = RunPage {
        section: "runs",
        events: format!("/jobs/{}/runs/{}/events?from={offset}", run.job, run.id),
        job: run.job.clone(),
        id: run.id,
        status: run_status(&run),
        class: status_class(run.status),
        started: human_time(&time::local(run.started_at), &now),
        took,
        trigger: run.trigger.as_str(),
        summary: summary(&run),
        command: run.command.clone(),
        lines: lines.iter().map(LineView::from).collect(),
        running,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub(super) struct EventsQuery {
    #[serde(default)]
    from: u64,
}

/// A live run's output as server-sent events: a `line` event for each line from byte `from` of its
/// log on, then an `end` event with how the run went. A run already over gets its remaining lines
/// and `end` at once.
pub(super) async fn events(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, i64)>,
    Query(query): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, PageError> {
    let run = state
        .run(move |macron| macron.find_run(&name, Some(id)))
        .await?;
    let (sender, receiver) = mpsc::channel(256);
    let inner = state.inner.clone();
    tokio::task::spawn_blocking(move || {
        stream_run(&inner.macron, run, query.from, &inner.stopping, &sender);
    });
    let events = ReceiverStream::new(receiver).map(Ok);
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

/// Sends `run`'s output as events until it ends, the page goes away, or the server stops.
fn stream_run(
    macron: &Macron,
    run: Run,
    from: u64,
    stopping: &watch::Receiver<bool>,
    sender: &mpsc::Sender<Event>,
) {
    let Ok(history) = macron.history() else {
        return;
    };
    let mut follower = macron.follow(run, from);
    while !*stopping.borrow() && !sender.is_closed() {
        let Ok((lines, ended)) = follower.poll(&history) else {
            return;
        };
        for line in &lines {
            let Ok(event) = Event::default()
                .event("line")
                .json_data(LineView::from(line))
            else {
                continue;
            };
            if sender.blocking_send(event).is_err() {
                return;
            }
        }
        if let Some(run) = ended {
            let _ = sender.blocking_send(Event::default().event("end").data(summary(&run)));
            return;
        }
        thread::sleep(POLL);
    }
}

#[derive(Template)]
#[template(path = "runs.html")]
struct RunsPage {
    section: &'static str,
    filters: Vec<FilterLink>,
    table: String,
}

struct FilterLink {
    label: &'static str,
    url: &'static str,
    current: bool,
}

#[derive(Deserialize)]
pub(super) struct RunsQuery {
    status: Option<String>,
}

pub(super) async fn runs(
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Html<String>, PageError> {
    let status = match query.status.as_deref() {
        Some("failed") => Some(Status::Failed),
        Some("running") => Some(Status::Running),
        Some("timed_out") => Some(Status::TimedOut),
        _ => None,
    };
    let runs = state
        .run(move |macron| {
            macron.history()?.runs(Filter {
                job: None,
                status,
                limit: RUNS_PAGE_RUNS,
            })
        })
        .await?;
    let now = Zoned::now();
    let table = RunsTable {
        rows: runs.iter().map(|run| RunRow::new(run, &now)).collect(),
        show_job: true,
        empty: if status.is_some() {
            "No runs match."
        } else {
            "Nothing has run yet."
        },
    }
    .render()?;
    let filters = [
        ("All", "/runs", None),
        ("Failed", "/runs?status=failed", Some(Status::Failed)),
        (
            "Timed out",
            "/runs?status=timed_out",
            Some(Status::TimedOut),
        ),
        ("Running", "/runs?status=running", Some(Status::Running)),
    ]
    .into_iter()
    .map(|(label, url, filter)| FilterLink {
        label,
        url,
        current: filter == status,
    })
    .collect();
    let page = RunsPage {
        section: "runs",
        filters,
        table,
    };
    Ok(Html(page.render()?))
}

/// `text` without terminal escape sequences, which colour a terminal but are noise on a page:
/// CSI sequences (`ESC [ … final`), OSC sequences (`ESC ] … BEL` or `ESC ] … ESC \`), and other
/// two-byte escapes.
fn strip_ansi(text: &str) -> Cow<'_, str> {
    if !text.contains('\x1b') {
        return Cow::Borrowed(text);
    }
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(char) = chars.next() {
        if char != '\x1b' {
            plain.push(char);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for char in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&char) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(char) = chars.next() {
                    if char == '\x07' {
                        break;
                    }
                    if char == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Cow::Owned(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_terminal_escapes() {
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[31merror\x1b[0m: x"), "error: x");
        assert_eq!(strip_ansi("\x1b[1;32mok\x1b[m"), "ok");
        assert_eq!(strip_ansi("\x1b]0;title\x07after"), "after");
        assert_eq!(
            strip_ansi("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
        assert_eq!(strip_ansi("a\x1bcb"), "ab");
    }
}
