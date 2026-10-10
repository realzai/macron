//! `macron doctor` end to end: a healthy sandbox, then one broken in each way it checks for.

mod support;

use std::fs;

use serde_json::Value;
use support::{LABEL_PREFIX, Sandbox};

fn diagnose(sandbox: &Sandbox) -> Value {
    sandbox.run(&["doctor", "--json"]).ok().json()
}

/// The checks named `check`, as (severity, message).
fn findings(diagnosis: &Value, check: &str) -> Vec<(String, String)> {
    diagnosis["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["check"] == check)
        .map(|finding| {
            (
                finding["severity"].as_str().unwrap().to_owned(),
                finding["message"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn add(sandbox: &Sandbox, name: &str) {
    sandbox
        .run(&[
            "add", name, "-s", "@daily", "--shell", "/bin/sh", "--", "true",
        ])
        .ok();
}

#[test]
fn a_healthy_setup_has_nothing_but_ok() {
    let sandbox = Sandbox::new();
    add(&sandbox, "backup");
    let diagnosis = diagnose(&sandbox);
    assert_eq!(diagnosis["healthy"], true);
    let severities: Vec<_> = diagnosis["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["severity"].as_str().unwrap())
        .collect();
    assert!(
        severities.iter().all(|severity| *severity == "ok"),
        "{diagnosis}"
    );
    assert!(
        diagnosis["notes"][0]
            .as_str()
            .unwrap()
            .contains("Full Disk Access")
    );

    let text = sandbox.run(&["doctor"]).ok().stdout;
    assert!(text.contains("✓ launchd matches jobs.toml"), "{text}");
    assert!(
        text.trim_end().ends_with("Everything looks fine."),
        "{text}"
    );
}

#[test]
fn reports_a_broken_jobs_toml() {
    let sandbox = Sandbox::new();
    sandbox.write_jobs_toml("[jobs.backup]\nschedule = \"@daily\"\n");
    let diagnosis = diagnose(&sandbox);
    assert_eq!(diagnosis["healthy"], false);
    let config = findings(&diagnosis, "config");
    assert_eq!(config[0].0, "problem");
    assert!(
        config[0].1.contains("missing field `command`"),
        "{}",
        config[0].1
    );
}

#[test]
fn warns_when_launchd_is_behind_jobs_toml() {
    let sandbox = Sandbox::new();
    sandbox.write_jobs_toml("[jobs.backup]\nschedule = \"@daily\"\ncommand = \"true\"\n");
    let diagnosis = diagnose(&sandbox);
    assert_eq!(diagnosis["healthy"], true, "a warning is not a problem");
    assert_eq!(
        findings(&diagnosis, "launchd"),
        [(
            "warning".to_owned(),
            "launchd does not match jobs.toml: install backup pending".to_owned()
        )]
    );
}

#[test]
fn reports_a_missing_shell_or_working_directory() {
    let sandbox = Sandbox::new();
    sandbox.write_jobs_toml(
        "[jobs.fish]\nschedule = \"@daily\"\ncommand = \"true\"\nshell = \"/nonexistent/fish\"\n\n\
         [jobs.lost]\nschedule = \"@daily\"\ncommand = \"true\"\nshell = \"/bin/sh\"\nworkdir = \"~/gone\"\n",
    );
    sandbox.run(&["apply"]).ok();
    let diagnosis = diagnose(&sandbox);
    assert_eq!(diagnosis["healthy"], false);
    let jobs = findings(&diagnosis, "jobs");
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    assert!(jobs[0].1.contains("`fish` runs in /nonexistent/fish"));
    assert!(
        jobs[1].1.contains("`lost` runs in")
            && jobs[1].1.ends_with("gone, which is not a directory")
    );
}

#[test]
fn reports_installed_jobs_that_run_a_missing_macron() {
    let sandbox = Sandbox::new();
    add(&sandbox, "backup");
    let plist = sandbox
        .agents_dir()
        .join(format!("{LABEL_PREFIX}backup.plist"));
    let text = fs::read_to_string(&plist).unwrap();
    let binary = env!("CARGO_BIN_EXE_macron");
    fs::write(&plist, text.replace(binary, "/nonexistent/macron")).unwrap();

    let diagnosis = diagnose(&sandbox);
    assert_eq!(diagnosis["healthy"], false);
    let binary_findings = findings(&diagnosis, "binary");
    assert_eq!(binary_findings[0].0, "problem");
    assert_eq!(
        binary_findings[0].1,
        "installed jobs `backup` run /nonexistent/macron, which no longer exists"
    );

    sandbox.run(&["apply"]).ok();
    assert_eq!(diagnose(&sandbox)["healthy"], true);
}

#[test]
fn warns_about_a_recent_problem_in_a_launchd_log() {
    let sandbox = Sandbox::new();
    add(&sandbox, "backup");
    let logs = sandbox.home().parent().unwrap().join("state/launchd");
    fs::create_dir_all(&logs).unwrap();
    fs::write(
        logs.join("backup.log"),
        "macron: cannot record this run of `backup`, so it runs unrecorded: disk full\n\n",
    )
    .unwrap();

    let launchd_log = findings(&diagnose(&sandbox), "launchd_log");
    assert_eq!(launchd_log[0].0, "warning");
    assert!(
        launchd_log[0]
            .1
            .ends_with("so it runs unrecorded: disk full"),
        "{launchd_log:?}"
    );

    let text = sandbox.run(&["doctor"]).ok().stdout;
    assert!(
        text.contains("! macron reported a problem running `backup`"),
        "{text}"
    );
    assert!(
        text.trim_end().ends_with("1 warning, and no problems."),
        "{text}"
    );
}
