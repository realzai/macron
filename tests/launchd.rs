//! Against the real launchd. These are ignored by default, because they install a job into
//! `~/Library/LaunchAgents` for a few seconds. Run them on a Mac before a release:
//!
//! ```sh
//! cargo test --test launchd -- --ignored
//! ```

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// Runs the binary against the real launchd and home directory, with its own config and state.
fn macron(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_macron"))
        .args(args)
        .env("MACRON_CONFIG_DIR", state.join("config"))
        .env("MACRON_STATE_DIR", state.join("state"))
        .env("MACRON_OSASCRIPT", "/usr/bin/true")
        .env_remove("MACRON_LAUNCHCTL")
        .env_remove("MACRON_LAUNCH_AGENTS_DIR")
        .output()
        .expect("macron should start")
}

/// Removes the job however the test ends, so a failure leaves nothing installed.
struct Installed<'a> {
    state: &'a Path,
    job: String,
}

impl Drop for Installed<'_> {
    fn drop(&mut self) {
        macron(self.state, &["rm", &self.job, "--yes", "--purge"]);
    }
}

#[test]
#[ignore = "installs a job into the real launchd"]
fn a_job_runs_through_the_real_launchd() {
    let state = TempDir::new().unwrap();
    let job = format!("macron-test-{}", std::process::id());
    let added = macron(
        state.path(),
        &[
            "add",
            &job,
            "-s",
            "@daily",
            "--shell",
            "/bin/sh",
            "--",
            "pwd; exit 3",
        ],
    );
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let installed = Installed {
        state: state.path(),
        job: job.clone(),
    };

    let run = macron(state.path(), &["run", &job, "--wait"]);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(run.status.code(), Some(3), "{stdout}");
    let home = std::env::var("HOME").unwrap();
    assert!(
        stdout.contains(&format!("  {home}\n")),
        "runs in the home directory:\n{stdout}"
    );
    assert!(stdout.contains("Failed with exit status 3"), "{stdout}");

    drop(installed);
    let plist = Path::new(&home).join(format!(
        "Library/LaunchAgents/io.github.realzai.macron.{job}.plist"
    ));
    assert!(!plist.exists(), "rm should have uninstalled the job");
}
