//! The pages for jobs: the list, one job, the form to add or change one, and the actions on it.

use askama::Template;
use axum::Form;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use jiff::Zoned;
use serde::{Deserialize, Serialize};

use super::forms::{FormErrors, JobForm};
use super::{AppState, PageError, tilde};
use crate::history::{Filter, Run, Status};
use crate::human::{ago, human_time, run_status, took};
use crate::launchd::State as JobState;
use crate::ops::{JobStatus, next_runs};
use crate::schedule::Schedule;
use crate::time;

/// How many runs a job's page lists.
const JOB_PAGE_RUNS: usize = 50;

/// How many upcoming runs the schedule preview and a job's page show.
const NEXT_RUNS: usize = 3;

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    title: &'a str,
    message: String,
    /// Whether to link back to the jobs, which a browser that is not signed in cannot see.
    back: bool,
}

/// A page saying what went wrong. Used for errors and for refused requests alike.
pub(super) fn error_page(title: &str, message: &str, back: bool) -> askama::Result<String> {
    ErrorPage {
        title,
        message: with_code(message),
        back,
    }
    .render()
}

/// `text` as a sentence: capitalised, with a full stop.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// `text` as HTML, escaped, with each `quoted` part in a `<code>` element, as macron's messages
/// mark names and commands.
fn with_code(text: &str) -> String {
    let mut html = String::with_capacity(text.len());
    let mut in_code = false;
    for char in text.chars() {
        match char {
            '`' => {
                html.push_str(if in_code { "</code>" } else { "<code>" });
                in_code = !in_code;
            }
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            '\'' => html.push_str("&#39;"),
            other => html.push(other),
        }
    }
    if in_code {
        html.push_str("</code>");
    }
    html
}

pub(super) async fn not_found() -> PageError {
    PageError::from(crate::error::Error::NotFound)
}

/// A run, as a status badge and how long ago it started.
pub(super) struct RunBadge {
    pub url: String,
    pub status: String,
    pub class: &'static str,
    pub ago: String,
}

impl RunBadge {
    pub fn new(run: &Run, now: &Zoned) -> Self {
        Self {
            url: format!("/jobs/{}/runs/{}", run.job, run.id),
            status: run_status(run),
            class: status_class(run.status),
            ago: ago(&time::local(run.started_at), now),
        }
    }
}

pub(super) fn status_class(status: Status) -> &'static str {
    match status {
        Status::Succeeded => "ok",
        Status::Failed | Status::TimedOut => "bad",
        Status::Running => "running",
        Status::Lost => "off",
    }
}

fn state_class(state: JobState) -> &'static str {
    match state {
        JobState::Active => "ok",
        JobState::Disabled => "off",
        JobState::NotInstalled
        | JobState::OutOfDate
        | JobState::NotLoaded
        | JobState::DisabledButInstalled => "warn",
    }
}

/// One row of the jobs table.
struct JobRow {
    name: String,
    description: Option<String>,
    schedule: String,
    when: String,
    next_run: String,
    last_run: Option<RunBadge>,
    state: &'static str,
    state_class: &'static str,
    can_run: bool,
}

impl JobRow {
    fn new(status: &JobStatus, now: &Zoned) -> Self {
        let job = &status.job;
        Self {
            name: job.name().to_owned(),
            description: job.description().map(str::to_owned),
            schedule: job.schedule_text().to_owned(),
            when: job.schedule().describe(),
            next_run: status
                .next_run
                .as_ref()
                .map_or_else(|| "-".to_owned(), |next| human_time(next, now)),
            last_run: status.last_run.as_ref().map(|run| RunBadge::new(run, now)),
            state: status.state.as_str(),
            state_class: state_class(status.state),
            can_run: status.state == JobState::Active,
        }
    }
}

/// The jobs table, which the jobs page refreshes on its own.
#[derive(Template)]
#[template(path = "jobs_table.html")]
struct JobsTable {
    rows: Vec<JobRow>,
    in_sync: bool,
    jobs_file: String,
}

#[derive(Template)]
#[template(path = "jobs.html")]
struct JobsPage {
    section: &'static str,
    table: String,
}

#[derive(Deserialize)]
pub(super) struct JobsQuery {
    partial: Option<String>,
}

pub(super) async fn jobs(
    State(state): State<AppState>,
    Query(query): Query<JobsQuery>,
) -> Result<Response, PageError> {
    let (overview, now) = state
        .run(|macron| {
            let now = Zoned::now();
            Ok((macron.overview(&now)?, now))
        })
        .await?;
    let table = JobsTable {
        rows: overview
            .jobs
            .iter()
            .map(|status| JobRow::new(status, &now))
            .collect(),
        in_sync: overview.in_sync,
        jobs_file: tilde(&state.paths().jobs_file, state.paths()),
    }
    .render()?;
    if query.partial.as_deref() == Some("jobs") {
        return Ok(Html(table).into_response());
    }
    let page = JobsPage {
        section: "jobs",
        table,
    };
    Ok(Html(page.render()?).into_response())
}

/// One row of a runs table.
pub(super) struct RunRow {
    pub id: i64,
    pub job: String,
    pub url: String,
    pub started: String,
    pub took: String,
    pub status: String,
    pub class: &'static str,
    pub trigger: &'static str,
}

impl RunRow {
    pub fn new(run: &Run, now: &Zoned) -> Self {
        let took = match run.duration_ms {
            Some(duration) => took(duration),
            None if run.status == Status::Running => {
                took(now.timestamp().as_millisecond() - run.started_at.as_millisecond())
            }
            None => "-".to_owned(),
        };
        Self {
            id: run.id,
            job: run.job.clone(),
            url: format!("/jobs/{}/runs/{}", run.job, run.id),
            started: human_time(&time::local(run.started_at), now),
            took,
            status: run_status(run),
            class: status_class(run.status),
            trigger: run.trigger.as_str(),
        }
    }
}

/// A table of runs, on a job's page or across every job.
#[derive(Template)]
#[template(path = "runs_table.html")]
pub(super) struct RunsTable {
    pub rows: Vec<RunRow>,
    /// Whether to name each run's job, which a job's own page does not need.
    pub show_job: bool,
    pub empty: &'static str,
}

#[derive(Template)]
#[template(path = "job.html")]
struct JobPage {
    section: &'static str,
    name: String,
    description: Option<String>,
    schedule: String,
    when: String,
    next_runs: String,
    state: &'static str,
    state_class: &'static str,
    enabled: bool,
    can_run: bool,
    command: String,
    workdir: String,
    shell: String,
    env: Vec<String>,
    timeout: String,
    notify: &'static str,
    keep_runs: u32,
    runs: String,
}

pub(super) async fn job(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Html<String>, PageError> {
    let (status, runs, now) = state
        .run(move |macron| {
            let now = Zoned::now();
            let status = macron.status(&name, &now)?;
            let runs = macron.history()?.runs(Filter {
                job: Some(&name),
                status: None,
                limit: JOB_PAGE_RUNS,
            })?;
            Ok((status, runs, now))
        })
        .await?;
    let job = &status.job;
    let next: Vec<String> = next_runs(job, &now)
        .take(NEXT_RUNS)
        .map(|time| human_time(&time, &now))
        .collect();
    let next_runs = match (next.is_empty(), job.enabled(), job.schedule()) {
        (false, _, _) => next.join(", "),
        (true, false, _) => "none while it is disabled".to_owned(),
        (true, true, Schedule::Every(_)) => "counted from when launchd loaded it".to_owned(),
        (true, true, Schedule::Calendar(_)) => "-".to_owned(),
    };
    let runs = RunsTable {
        rows: runs.iter().map(|run| RunRow::new(run, &now)).collect(),
        show_job: false,
        empty: "It has not run yet.",
    }
    .render()?;
    let page = JobPage {
        section: "jobs",
        name: job.name().to_owned(),
        description: job.description().map(str::to_owned),
        schedule: job.schedule_text().to_owned(),
        when: job.schedule().describe(),
        next_runs,
        state: status.state.as_str(),
        state_class: state_class(status.state),
        enabled: job.enabled(),
        can_run: status.state == JobState::Active,
        command: job.command().to_owned(),
        workdir: job.workdir().to_owned(),
        shell: job.shell().display().to_string(),
        env: job
            .env()
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
        timeout: job
            .timeout()
            .map_or_else(|| "none".to_owned(), |timeout| timeout.to_string()),
        notify: job.notify().as_str(),
        keep_runs: job.keep_runs(),
        runs,
    };
    Ok(Html(page.render()?))
}

#[derive(Template)]
#[template(path = "job_form.html")]
struct JobFormPage {
    section: &'static str,
    form: JobForm,
    errors: FormErrors,
    /// Whether this changes a job rather than adding one.
    editing: bool,
    /// The schedule in words, when it is valid.
    when: String,
}

impl JobFormPage {
    fn new(form: JobForm, errors: FormErrors, editing: bool) -> Self {
        let when = form
            .schedule
            .parse::<Schedule>()
            .map(|schedule| sentence(&schedule.describe()))
            .unwrap_or_default();
        Self {
            section: "jobs",
            form,
            errors,
            editing,
            when,
        }
    }

    /// Whether to open the optional fields: when one is set, or has an error.
    fn more_open(&self) -> bool {
        let form = &self.form;
        let set = [
            &form.workdir,
            &form.shell,
            &form.env,
            &form.timeout,
            &form.notify,
            &form.keep_runs,
        ]
        .iter()
        .any(|field| !field.trim().is_empty());
        let errors = ["workdir", "shell", "env", "timeout", "notify", "keep_runs"]
            .iter()
            .any(|field| !self.errors.get(field).is_empty());
        set || errors
    }

    fn respond(self, status: StatusCode) -> Result<Response, PageError> {
        Ok((status, Html(self.render()?)).into_response())
    }
}

pub(super) async fn new_job() -> Result<Response, PageError> {
    JobFormPage::new(JobForm::blank(), FormErrors::default(), false).respond(StatusCode::OK)
}

pub(super) async fn create(
    State(state): State<AppState>,
    Form(form): Form<JobForm>,
) -> Result<Response, PageError> {
    let spec = match form.to_spec() {
        Ok(spec) => spec,
        Err(errors) => {
            return JobFormPage::new(form, errors, false).respond(StatusCode::BAD_REQUEST);
        }
    };
    let name = form.name.trim().to_owned();
    let added = {
        let name = name.clone();
        state.try_run(move |macron| macron.add(&name, &spec)).await
    };
    match added {
        Ok(_) => Ok(Redirect::to(&format!("/jobs/{name}")).into_response()),
        Err(err) => JobFormPage::new(form, FormErrors::from_error(&err), false)
            .respond(StatusCode::BAD_REQUEST),
    }
}

pub(super) async fn edit_job(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, PageError> {
    let spec = {
        let name = name.clone();
        state
            .run(move |macron| Ok(macron.config()?.job(&name)?.spec().clone()))
            .await?
    };
    JobFormPage::new(
        JobForm::from_spec(&name, &spec),
        FormErrors::default(),
        true,
    )
    .respond(StatusCode::OK)
}

pub(super) async fn update(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Form(mut form): Form<JobForm>,
) -> Result<Response, PageError> {
    // The name is in the address; the form has no field for it, since a job cannot be renamed.
    form.name.clone_from(&name);
    let spec = match form.to_spec() {
        Ok(spec) => spec,
        Err(errors) => {
            return JobFormPage::new(form, errors, true).respond(StatusCode::BAD_REQUEST);
        }
    };
    let edited = {
        let name = name.clone();
        state
            .try_run(move |macron| macron.edit(&name, move |current| *current = spec))
            .await
    };
    match edited {
        Ok(_) => Ok(Redirect::to(&format!("/jobs/{name}")).into_response()),
        Err(err) => JobFormPage::new(form, FormErrors::from_error(&err), true)
            .respond(StatusCode::BAD_REQUEST),
    }
}

/// Where to go back to after an action: the page it was taken from, if it says, and otherwise the
/// job's own page. Only paths on this server are followed.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(super) struct Back {
    back: String,
}

impl Back {
    fn to(&self, name: &str) -> Redirect {
        let local = self.back.starts_with('/') && !self.back.starts_with("//");
        if local {
            Redirect::to(&self.back)
        } else {
            Redirect::to(&format!("/jobs/{name}"))
        }
    }
}

pub(super) async fn run_now(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Redirect, PageError> {
    let run = state.run(move |macron| macron.start_run(&name)).await?;
    Ok(Redirect::to(&format!("/jobs/{}/runs/{}", run.job, run.id)))
}

pub(super) async fn enable(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Form(back): Form<Back>,
) -> Result<Redirect, PageError> {
    set_enabled(state, name, back, true).await
}

pub(super) async fn disable(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Form(back): Form<Back>,
) -> Result<Redirect, PageError> {
    set_enabled(state, name, back, false).await
}

async fn set_enabled(
    state: AppState,
    name: String,
    back: Back,
    enabled: bool,
) -> Result<Redirect, PageError> {
    let redirect = back.to(&name);
    state
        .run(move |macron| macron.set_enabled(&name, enabled))
        .await?;
    Ok(redirect)
}

#[derive(Template)]
#[template(path = "delete.html")]
struct DeletePage {
    section: &'static str,
    name: String,
    runs: usize,
}

pub(super) async fn confirm_delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Html<String>, PageError> {
    let runs = {
        let name = name.clone();
        state
            .run(move |macron| {
                macron.config()?.job(&name)?;
                macron.history()?.count(&name)
            })
            .await?
    };
    let page = DeletePage {
        section: "jobs",
        name,
        runs,
    };
    Ok(Html(page.render()?))
}

#[derive(Deserialize)]
pub(super) struct DeleteForm {
    purge: Option<String>,
}

pub(super) async fn delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Form(form): Form<DeleteForm>,
) -> Result<Redirect, PageError> {
    let purge = form.purge.is_some();
    state.run(move |macron| macron.remove(&name, purge)).await?;
    Ok(Redirect::to("/"))
}

pub(super) async fn apply(State(state): State<AppState>) -> Result<Redirect, PageError> {
    state.run(|macron| macron.apply(false)).await?;
    Ok(Redirect::to("/"))
}

#[derive(Deserialize)]
pub(super) struct ScheduleQuery {
    #[serde(default)]
    text: String,
}

/// A schedule in words, with its next runs, or why it is invalid: what the form shows as you type.
#[derive(Serialize)]
pub(super) struct Described {
    description: Option<String>,
    next_runs: Vec<String>,
    error: Option<String>,
}

pub(super) async fn describe_schedule(Query(query): Query<ScheduleQuery>) -> Json<Described> {
    let described = match query.text.parse::<Schedule>() {
        Ok(schedule) => {
            let now = Zoned::now();
            let next = std::iter::successors(schedule.next_after(&now), |previous| {
                schedule.next_after(previous)
            });
            Described {
                description: Some(schedule.describe()),
                next_runs: next
                    .take(NEXT_RUNS)
                    .map(|time| human_time(&time, &now))
                    .collect(),
                error: None,
            }
        }
        Err(crate::error::Error::InvalidSchedule { reason, .. }) => Described {
            description: None,
            next_runs: Vec::new(),
            error: Some(reason),
        },
        Err(err) => Described {
            description: None,
            next_runs: Vec::new(),
            error: Some(err.to_string()),
        },
    };
    Json(described)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_description_a_sentence() {
        assert_eq!(sentence("at 09:00 every day"), "At 09:00 every day.");
        assert_eq!(sentence(""), "");
    }

    #[test]
    fn escapes_messages_and_marks_code() {
        assert_eq!(
            with_code("there is no job named `<b>`"),
            "there is no job named <code>&lt;b&gt;</code>"
        );
        assert_eq!(
            with_code("a \"quote\" & 'x'"),
            "a &quot;quote&quot; &amp; &#39;x&#39;"
        );
        assert_eq!(with_code("unclosed `code"), "unclosed <code>code</code>");
    }
}
