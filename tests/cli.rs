//! The command-line contract: what scripts and agents can rely on. Each test runs the built binary
//! with a cleared environment, so nothing on the machine running the tests leaks in.

use std::process::{Command, Output};

use serde_json::Value;

fn macron(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_macron"))
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .expect("macron should start")
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("output should be JSON")
}

#[test]
fn paths_json_reports_every_location() {
    let out = macron(
        &["paths", "--json"],
        &[("HOME", "/Users/ada"), ("MACRON_STATE_DIR", "/tmp/m")],
    );
    assert!(out.status.success());
    let paths = json(&out.stdout);
    assert_eq!(paths["jobs_file"], "/Users/ada/.config/macron/jobs.toml");
    assert_eq!(paths["state_dir"], "/tmp/m");
    assert_eq!(paths["history_db"], "/tmp/m/history.db");
    assert_eq!(paths["logs_dir"], "/tmp/m/logs");
    assert_eq!(paths["launchd_logs_dir"], "/tmp/m/launchd");
    assert_eq!(
        paths["launch_agents_dir"],
        "/Users/ada/Library/LaunchAgents"
    );
}

#[test]
fn with_json_a_failure_is_a_json_error_on_stderr() {
    let out = macron(&["--json", "paths"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(json(&out.stderr)["error"]["code"], "no_home");
}

#[test]
fn with_json_a_usage_error_is_a_json_error() {
    let out = macron(&["frobnicate", "--json"], &[("HOME", "/Users/ada")]);
    assert_eq!(out.status.code(), Some(2));
    let error = json(&out.stderr);
    assert_eq!(error["error"]["code"], "usage");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("frobnicate")
    );
}

#[test]
fn without_json_a_usage_error_is_text() {
    let out = macron(&["frobnicate"], &[("HOME", "/Users/ada")]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unrecognized subcommand"));
}
