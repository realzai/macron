//! Managing jobs end to end: each command, run as the built binary, against `jobs.toml`, the
//! LaunchAgents directory and a stand-in `launchctl`.

mod support;

use std::fs;

use support::{Sandbox, label};

fn add_backup(sandbox: &Sandbox) {
    sandbox
        .run(&[
            "add",
            "backup",
            "-s",
            "0 9 1 * *",
            "--",
            "echo",
            "hello world",
        ])
        .ok();
}

#[test]
fn add_writes_the_job_then_installs_and_loads_it() {
    let sandbox = Sandbox::new();
    let out = sandbox
        .run(&[
            "add",
            "backup",
            "-s",
            "0 9 1 * *",
            "--timeout",
            "1h",
            "--json",
            "--",
            "echo",
            "hello world",
        ])
        .ok()
        .json();

    assert_eq!(out["job"]["name"], "backup");
    assert_eq!(out["job"]["command"], "echo 'hello world'");
    assert_eq!(
        out["job"]["schedule_description"],
        "at 09:00 on the 1st of every month"
    );
    assert_eq!(out["job"]["timeout"], "1h");
    assert_eq!(out["job"]["state"], "active");
    assert!(out["job"]["next_run"].is_string());
    assert_eq!(out["changes"][0]["action"], "install");
    assert_eq!(out["changes"][0]["label"], label("backup"));

    assert_eq!(
        sandbox.jobs_toml(),
        "[jobs.backup]\nschedule = \"0 9 1 * *\"\ncommand = \"echo 'hello world'\"\ntimeout = \"1h\"\n"
    );
    let plist = sandbox
        .plist("backup")
        .expect("the plist should be installed");
    assert!(plist.contains("<string>exec</string>\n\t\t<string>backup</string>"));
    assert!(plist.contains("<key>Day</key>\n\t\t\t<integer>1</integer>"));
    assert_eq!(sandbox.loaded(), [label("backup")]);
}

#[test]
fn add_disabled_writes_the_job_but_installs_nothing() {
    let sandbox = Sandbox::new();
    let out = sandbox
        .run(&[
            "add",
            "backup",
            "-s",
            "@daily",
            "--disabled",
            "--json",
            "--",
            "true",
        ])
        .ok()
        .json();
    assert_eq!(out["job"]["state"], "disabled");
    assert_eq!(out["job"]["next_run"], serde_json::Value::Null);
    assert!(sandbox.jobs_toml().contains("enabled = false"));
    assert_eq!(sandbox.plist("backup"), None);
    assert!(sandbox.loaded().is_empty());
}

#[test]
fn add_refuses_duplicate_and_invalid_jobs_without_changing_anything() {
    let sandbox = Sandbox::new();
    add_backup(&sandbox);
    let before = sandbox.jobs_toml();
    let refuse = |args: &[&str]| sandbox.run(args).error_code();

    assert_eq!(
        refuse(&["add", "backup", "-s", "@daily", "--json", "--", "true"]),
        "job_exists"
    );
    assert_eq!(
        refuse(&["add", "new", "-s", "0 25 * * *", "--json", "--", "true"]),
        "invalid_schedule"
    );
    assert_eq!(
        refuse(&["add", "New", "-s", "@daily", "--json", "--", "true"]),
        "invalid_job_name"
    );
    assert_eq!(
        refuse(&[
            "add",
            "new",
            "-s",
            "@daily",
            "--timeout",
            "soon",
            "--json",
            "--",
            "true"
        ]),
        "invalid_duration"
    );
    assert_eq!(
        refuse(&[
            "add", "new", "-s", "@daily", "--shell", "zsh", "--json", "--", "true"
        ]),
        "invalid_job"
    );
    assert_eq!(sandbox.jobs_toml(), before);
    assert_eq!(sandbox.loaded(), [label("backup")]);
}

#[test]
fn ls_shows_each_job_with_its_state_and_next_run() {
    let sandbox = Sandbox::new();
    add_backup(&sandbox);
    sandbox
        .run(&["add", "sweep", "-s", "@every 15m", "--", "true"])
        .ok();
    sandbox
        .run(&["add", "old", "-s", "@daily", "--disabled", "--", "true"])
        .ok();

    let out = sandbox.run(&["ls", "--json"]).ok().json();
    let jobs = out["jobs"].as_array().unwrap();
    let names: Vec<_> = jobs
        .iter()
        .map(|job| job["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["backup", "old", "sweep"]);
    let states: Vec<_> = jobs
        .iter()
        .map(|job| job["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["active", "disabled", "active"]);
    assert!(jobs[0]["next_run"].is_string());
    assert!(jobs[1]["next_run"].is_null(), "disabled jobs do not run");
    assert!(
        jobs[2]["next_run"].is_null(),
        "launchd counts @every from load"
    );
    assert_eq!(out["in_sync"], true);

    let text = sandbox.run(&["ls"]).ok().stdout;
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap().split_whitespace().collect::<Vec<_>>(),
        ["NAME", "SCHEDULE", "NEXT", "RUN", "STATE", "WHEN"]
    );
    assert!(lines.next().unwrap().starts_with("backup  0 9 1 * *"));
}

#[test]
fn ls_with_no_jobs_says_how_to_add_one() {
    let sandbox = Sandbox::new();
    assert_eq!(
        sandbox.run(&["ls", "--json"]).ok().json()["jobs"],
        serde_json::json!([])
    );
    assert!(sandbox.run(&["ls"]).ok().stdout.starts_with("No jobs yet."));
}

#[test]
fn hand_edits_show_as_pending_until_applied() {
    let sandbox = Sandbox::new();
    sandbox.write_jobs_toml("[jobs.backup]\nschedule = \"@daily\"\ncommand = \"true\"\n");

    let listing = sandbox.run(&["ls", "--json"]).ok().json();
    assert_eq!(listing["jobs"][0]["state"], "not_installed");
    assert_eq!(listing["in_sync"], false);
    assert!(
        sandbox
            .run(&["ls"])
            .ok()
            .stdout
            .contains("Run `macron apply`")
    );

    let dry_run = sandbox.run(&["apply", "--dry-run", "--json"]).ok().json();
    assert_eq!(dry_run["dry_run"], true);
    assert_eq!(dry_run["changes"][0]["action"], "install");
    assert_eq!(sandbox.plist("backup"), None);
    assert!(sandbox.loaded().is_empty());

    assert_eq!(sandbox.run(&["apply"]).ok().stdout, "installed backup\n");
    assert_eq!(sandbox.loaded(), [label("backup")]);
    assert_eq!(
        sandbox.run(&["apply"]).ok().stdout,
        "Nothing to do: launchd already matches jobs.toml.\n"
    );
}

#[test]
fn apply_loads_an_installed_plist_launchd_has_dropped() {
    let sandbox = Sandbox::new();
    add_backup(&sandbox);
    fs::remove_dir_all(sandbox.home().parent().unwrap().join("launchctl/loaded")).unwrap();

    let out = sandbox.run(&["ls", "--json"]).ok().json();
    assert_eq!(out["jobs"][0]["state"], "not_loaded");
    assert_eq!(sandbox.run(&["apply"]).ok().stdout, "loaded backup\n");
    assert_eq!(sandbox.loaded(), [label("backup")]);
}

#[test]
fn apply_removes_its_own_orphans_and_leaves_other_agents_alone() {
    let sandbox = Sandbox::new();
    let agents = sandbox.agents_dir();
    fs::create_dir_all(&agents).unwrap();
    fs::write(agents.join("com.example.other.plist"), "theirs").unwrap();
    fs::write(agents.join(format!("{}.plist", label("ghost"))), "ours").unwrap();

    assert_eq!(sandbox.run(&["apply"]).ok().stdout, "removed ghost\n");
    assert!(!agents.join(format!("{}.plist", label("ghost"))).exists());
    assert_eq!(
        fs::read_to_string(agents.join("com.example.other.plist")).unwrap(),
        "theirs"
    );
}

#[test]
fn edit_changes_only_what_it_is_given_and_reinstalls() {
    let sandbox = Sandbox::new();
    sandbox.write_jobs_toml(
        "# Backups.\n[jobs.backup]\nschedule = \"0 9 1 * *\" # monthly\ncommand = \"true\"\ntimeout = \"1h\"\n",
    );
    sandbox.run(&["apply"]).ok();

    let out = sandbox
        .run(&[
            "edit",
            "backup",
            "-s",
            "@every 15m",
            "--unset",
            "timeout",
            "-e",
            "A=1",
            "--json",
        ])
        .ok()
        .json();
    assert_eq!(out["changes"][0]["action"], "update");
    assert_eq!(out["job"]["timeout"], serde_json::Value::Null);
    assert_eq!(
        sandbox.jobs_toml(),
        "# Backups.\n[jobs.backup]\nschedule = \"@every 15m\" # monthly\ncommand = \"true\"\nenv = { A = \"1\" }\n"
    );
    let plist = sandbox.plist("backup").unwrap();
    assert!(plist.contains("<key>StartInterval</key>\n\t<integer>900</integer>"));
    assert_eq!(sandbox.loaded(), [label("backup")]);
}

#[test]
fn edit_with_nothing_new_changes_nothing() {
    let sandbox = Sandbox::new();
    add_backup(&sandbox);
    let before = sandbox.jobs_toml();
    let out = sandbox
        .run(&["edit", "backup", "-s", "0 9 1 * *", "--json"])
        .ok()
        .json();
    assert_eq!(out["changes"], serde_json::json!([]));
    assert_eq!(sandbox.jobs_toml(), before);
}

#[test]
fn disable_uninstalls_and_enable_reinstalls() {
    let sandbox = Sandbox::new();
    add_backup(&sandbox);

    let out = sandbox.run(&["disable", "backup", "--json"]).ok().json();
    assert_eq!(out["changes"][0]["action"], "remove");
    assert!(sandbox.jobs_toml().contains("enabled = false"));
    assert_eq!(sandbox.plist("backup"), None);
    assert!(sandbox.loaded().is_empty());

    let out = sandbox.run(&["enable", "backup", "--json"]).ok().json();
    assert_eq!(out["changes"][0]["action"], "install");
    assert!(!sandbox.jobs_toml().contains("enabled"));
    assert_eq!(sandbox.loaded(), [label("backup")]);
}

#[test]
fn rm_needs_yes_without_a_terminal() {
    let sandbox = Sandbox::new();
    add_backup(&sandbox);

    assert_eq!(
        sandbox.run(&["rm", "backup", "--json"]).error_code(),
        "confirmation_required"
    );
    assert!(sandbox.jobs_toml().contains("[jobs.backup]"));

    let out = sandbox
        .run(&["rm", "backup", "--yes", "--json"])
        .ok()
        .json();
    assert_eq!(out["removed"], "backup");
    assert_eq!(out["changes"][0]["action"], "remove");
    assert!(!sandbox.jobs_toml().contains("backup"));
    assert_eq!(sandbox.plist("backup"), None);
    assert!(sandbox.loaded().is_empty());
}

#[test]
fn show_gives_the_job_in_full() {
    let sandbox = Sandbox::new();
    add_backup(&sandbox);
    let out = sandbox.run(&["show", "backup", "--json"]).ok().json();
    assert_eq!(out["name"], "backup");
    assert_eq!(out["label"], label("backup"));
    assert_eq!(out["next_runs"].as_array().unwrap().len(), 5);
    assert_eq!(out["next_runs"][0], out["next_run"]);
    assert!(
        out["plist"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{}.plist", label("backup")))
    );

    let text = sandbox.run(&["show", "backup"]).ok().stdout;
    assert!(text.starts_with("backup\n"));
    assert!(text.contains("schedule     0 9 1 * *  (at 09:00 on the 1st of every month)"));
}

#[test]
fn a_missing_job_is_an_error_in_text_and_json() {
    let sandbox = Sandbox::new();
    let out = sandbox.run(&["show", "nope"]);
    assert_eq!(out.status, Some(1));
    assert_eq!(out.stderr, "macron: there is no job named `nope`\n");
    assert_eq!(
        sandbox.run(&["show", "nope", "--json"]).error_code(),
        "no_such_job"
    );
}

#[test]
fn exec_runs_the_job_and_exits_as_it_did() {
    let sandbox = Sandbox::new();
    sandbox
        .run(&[
            "add",
            "greet",
            "-s",
            "@daily",
            "--shell",
            "/bin/sh",
            "-e",
            "GREETING=hello",
            "--",
            "printf %s \"$GREETING\" > out.txt; exit 3",
        ])
        .ok();
    let out = sandbox.run(&["exec", "greet"]);
    assert_eq!(out.status, Some(3));
    assert_eq!(
        fs::read_to_string(sandbox.home().join("out.txt")).unwrap(),
        "hello"
    );
}

#[test]
fn exec_reports_a_timeout_and_exits_124() {
    let sandbox = Sandbox::new();
    sandbox
        .run(&[
            "add",
            "slow",
            "-s",
            "@daily",
            "--shell",
            "/bin/sh",
            "--timeout",
            "1s",
            "--",
            "sleep 30",
        ])
        .ok();
    let out = sandbox.run(&["exec", "slow"]);
    assert_eq!(out.status, Some(124));
    assert_eq!(
        out.stderr,
        "macron: stopped `slow` when it reached its timeout of 1s\n"
    );
}
