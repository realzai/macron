//! Runs end to end: recording them through `exec`, starting them with `run`, and reading them back
//! with `runs`, `logs`, `ls` and `show`.

mod support;

use std::fs;

use serde_json::Value;
use support::Sandbox;

/// Adds a job with a POSIX shell, so tests do not depend on the machine's zsh profile.
fn add(sandbox: &Sandbox, name: &str, command: &str, options: &[&str]) {
    let mut args = vec!["add", name, "-s", "@daily", "--shell", "/bin/sh"];
    args.extend_from_slice(options);
    args.extend(["--", command]);
    sandbox.run(&args).ok();
}

fn runs(sandbox: &Sandbox, args: &[&str]) -> Vec<Value> {
    let mut all = vec!["runs", "--json"];
    all.extend_from_slice(args);
    sandbox.run(&all).ok().json()["runs"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn exec_records_the_run_and_its_output() {
    let sandbox = Sandbox::new();
    add(&sandbox, "greet", "echo hello; echo oops >&2; exit 2", &[]);

    assert_eq!(sandbox.run(&["exec", "greet"]).status, Some(2));

    let runs = runs(&sandbox, &[]);
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run["job"], "greet");
    assert_eq!(run["status"], "failed");
    assert_eq!(run["exit_code"], 2);
    assert_eq!(run["trigger"], "schedule");
    assert_eq!(run["command"], "echo hello; echo oops >&2; exit 2");
    assert!(run["started_at"].is_string() && run["finished_at"].is_string());

    let log = sandbox.run(&["logs", "greet", "--json"]).ok().json();
    assert_eq!(log["run"]["id"], run["id"]);
    let lines: Vec<_> = log["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| {
            (
                line["stream"].as_str().unwrap(),
                line["text"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(lines.contains(&("out", "hello")) && lines.contains(&("err", "oops")));

    assert_eq!(
        sandbox.notifications(),
        ["greet failed with exit status 2 after less than a second"]
    );
}

#[test]
fn logs_prints_for_people_or_raw() {
    let sandbox = Sandbox::new();
    add(&sandbox, "greet", "echo hello", &[]);
    sandbox.run(&["exec", "greet"]).ok();

    assert_eq!(
        sandbox.run(&["logs", "greet", "--raw"]).ok().stdout,
        "hello\n"
    );

    let text = sandbox.run(&["logs", "greet"]).ok().stdout;
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].starts_with("Run 1 of greet, started "));
    assert!(lines[0].ends_with(" on schedule."));
    assert!(lines[1].ends_with("  hello"));
    assert_eq!(lines[2], "Succeeded in <1s.");
}

#[test]
fn run_starts_the_job_through_launchd_as_a_manual_run() {
    let sandbox = Sandbox::new();
    add(&sandbox, "greet", "echo hello", &["--notify", "always"]);

    let started = sandbox.run(&["run", "greet", "--json"]).ok().json();
    assert_eq!(started["trigger"], "manual");
    let id = started["id"].as_i64().unwrap();

    let followed = sandbox
        .run(&["logs", "greet", "--follow", "--json"])
        .ok()
        .stdout;
    let objects: Vec<Value> = followed
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(objects[0]["text"], "hello");
    let ended = &objects.last().unwrap()["run"];
    assert_eq!(ended["id"], id);
    assert_eq!(ended["status"], "succeeded");
    assert_eq!(
        sandbox.notifications(),
        ["greet succeeded in less than a second"]
    );
}

#[test]
fn run_wait_prints_the_output_and_exits_as_the_job_did() {
    let sandbox = Sandbox::new();
    add(&sandbox, "greet", "echo hello; exit 3", &[]);

    let out = sandbox.run(&["run", "greet", "--wait"]);
    assert_eq!(out.status, Some(3));
    let lines: Vec<_> = out.stdout.lines().collect();
    assert_eq!(lines[0], "Started greet as run 1.");
    assert!(lines[1].ends_with("  hello"));
    assert_eq!(lines[2], "Failed with exit status 3 after <1s.");

    let finished = sandbox.run(&["run", "greet", "--wait", "--json"]);
    assert_eq!(finished.status, Some(3));
    let run: Value = serde_json::from_str(&finished.stdout).unwrap();
    assert_eq!(run["id"], 2);
    assert_eq!(run["exit_code"], 3);
}

#[test]
fn run_refuses_what_launchd_cannot_start() {
    let sandbox = Sandbox::new();
    add(&sandbox, "off", "true", &["--disabled"]);
    assert_eq!(
        sandbox.run(&["run", "off", "--json"]).error_code(),
        "job_disabled"
    );

    sandbox.write_jobs_toml(&format!(
        "{}[jobs.unapplied]\nschedule = \"@daily\"\ncommand = \"true\"\n",
        sandbox.jobs_toml()
    ));
    assert_eq!(
        sandbox.run(&["run", "unapplied", "--json"]).error_code(),
        "not_loaded"
    );
    assert_eq!(
        sandbox.run(&["run", "nope", "--json"]).error_code(),
        "no_such_job"
    );
}

#[test]
fn run_refuses_a_job_that_is_already_running() {
    let sandbox = Sandbox::new();
    add(&sandbox, "slow", "sleep 3", &[]);
    sandbox.run(&["run", "slow"]).ok();
    let out = sandbox.run(&["run", "slow", "--json"]);
    assert_eq!(out.error_code(), "already_running");
    let finished = sandbox
        .run(&["logs", "slow", "--follow", "--json"])
        .ok()
        .stdout;
    assert!(finished.contains("\"succeeded\""));
}

#[test]
fn runs_filters_by_job_and_status_newest_first() {
    let sandbox = Sandbox::new();
    add(&sandbox, "good", "true", &[]);
    add(&sandbox, "bad", "false", &[]);
    for job in ["good", "bad", "good"] {
        sandbox.run(&["exec", job]);
    }
    let ids = |runs: Vec<Value>| -> Vec<i64> {
        runs.iter().map(|run| run["id"].as_i64().unwrap()).collect()
    };
    assert_eq!(ids(runs(&sandbox, &[])), [3, 2, 1]);
    assert_eq!(ids(runs(&sandbox, &["good"])), [3, 1]);
    assert_eq!(ids(runs(&sandbox, &["--status", "failed"])), [2]);
    assert_eq!(ids(runs(&sandbox, &["--limit", "1"])), [3]);

    let text = sandbox.run(&["runs"]).ok().stdout;
    let header: Vec<_> = text.lines().next().unwrap().split_whitespace().collect();
    assert_eq!(
        header,
        ["RUN", "JOB", "STARTED", "TOOK", "STATUS", "TRIGGER"]
    );
    assert!(text.contains("failed, exit 1"));
}

#[test]
fn ls_and_show_report_the_last_run() {
    let sandbox = Sandbox::new();
    add(&sandbox, "greet", "true", &[]);
    let listing = sandbox.run(&["ls", "--json"]).ok().json();
    assert!(listing["jobs"][0]["last_run"].is_null());

    sandbox.run(&["exec", "greet"]).ok();
    let listing = sandbox.run(&["ls", "--json"]).ok().json();
    assert_eq!(listing["jobs"][0]["last_run"]["status"], "succeeded");
    assert!(
        sandbox
            .run(&["ls"])
            .ok()
            .stdout
            .contains("succeeded just now")
    );

    let shown = sandbox.run(&["show", "greet", "--json"]).ok().json();
    assert_eq!(shown["last_run"]["id"], 1);
    assert!(
        sandbox
            .run(&["show", "greet"])
            .ok()
            .stdout
            .contains("(run 1)")
    );
}

#[test]
fn logs_explains_what_it_cannot_find() {
    let sandbox = Sandbox::new();
    add(&sandbox, "greet", "true", &[]);
    add(&sandbox, "other", "true", &[]);
    assert_eq!(
        sandbox.run(&["logs", "greet", "--json"]).error_code(),
        "no_runs"
    );
    assert_eq!(
        sandbox.run(&["logs", "nope", "--json"]).error_code(),
        "no_such_job"
    );

    sandbox.run(&["exec", "other"]).ok();
    assert_eq!(
        sandbox
            .run(&["logs", "greet", "--run", "1", "--json"])
            .error_code(),
        "no_such_run"
    );
}

#[test]
fn rm_keeps_the_history_unless_purged() {
    let sandbox = Sandbox::new();
    add(&sandbox, "kept", "true", &[]);
    add(&sandbox, "purged", "true", &[]);
    sandbox.run(&["exec", "kept"]).ok();
    sandbox.run(&["exec", "purged"]).ok();
    sandbox.run(&["exec", "purged"]).ok();

    sandbox.run(&["rm", "kept", "--yes"]).ok();
    assert_eq!(runs(&sandbox, &["kept"]).len(), 1);
    assert_eq!(sandbox.run(&["logs", "kept", "--raw"]).status, Some(0));

    let out = sandbox
        .run(&["rm", "purged", "--yes", "--purge", "--json"])
        .ok()
        .json();
    assert_eq!(out["purged_runs"], 2);
    assert!(runs(&sandbox, &["purged"]).is_empty());
    let logs = sandbox.home().parent().unwrap().join("state/logs/purged");
    assert!(!logs.exists());
    assert!(
        fs::read_dir(logs.parent().unwrap().join("kept"))
            .unwrap()
            .next()
            .is_some()
    );
}
