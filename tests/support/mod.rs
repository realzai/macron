//! A sandbox for running the built binary: its own home, config, state and LaunchAgents
//! directories, and a stand-in for `launchctl`, so tests never touch the machine running them.

// Each test file uses its own share of these helpers.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

pub const LABEL_PREFIX: &str = "io.github.realzai.macron.";

const FAKE_LAUNCHCTL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/fake-launchctl");
const FAKE_OSASCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/fake-osascript");

pub struct Sandbox {
    root: TempDir,
}

/// How one run of the binary went.
pub struct Run {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|err| {
            panic!(
                "stdout is not JSON ({err}):\n{}\n{}",
                self.stdout, self.stderr
            )
        })
    }

    /// The error code from a `--json` failure, checking it failed with status 1.
    pub fn error_code(&self) -> String {
        assert_eq!(self.status, Some(1), "expected a failure:\n{}", self.stdout);
        let error: Value = serde_json::from_str(&self.stderr)
            .unwrap_or_else(|err| panic!("stderr is not JSON ({err}):\n{}", self.stderr));
        error["error"]["code"].as_str().unwrap().to_owned()
    }

    /// Checks the run succeeded, and returns it.
    pub fn ok(self) -> Self {
        assert_eq!(
            self.status,
            Some(0),
            "expected success:\nstdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }
}

impl Sandbox {
    pub fn new() -> Self {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join("home")).unwrap();
        Self { root }
    }

    pub fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.root.path().join("agents")
    }

    pub fn jobs_file(&self) -> PathBuf {
        self.root.path().join("config/jobs.toml")
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let root = self.root.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_macron"));
        command
            .args(args)
            .env_clear()
            .env("HOME", self.home())
            .env("PATH", "/usr/bin:/bin")
            .env("MACRON_CONFIG_DIR", root.join("config"))
            .env("MACRON_STATE_DIR", root.join("state"))
            .env("MACRON_LAUNCH_AGENTS_DIR", self.agents_dir())
            .env("MACRON_LAUNCHCTL", FAKE_LAUNCHCTL)
            .env("FAKE_LAUNCHCTL_STATE", root.join("launchctl"))
            .env("FAKE_MACRON_BIN", env!("CARGO_BIN_EXE_macron"))
            .env("MACRON_OSASCRIPT", FAKE_OSASCRIPT)
            .env("FAKE_NOTIFICATIONS", root.join("notifications"));
        command
    }

    pub fn run(&self, args: &[&str]) -> Run {
        let output = self.command(args).output().expect("macron should start");
        Run {
            status: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    pub fn jobs_toml(&self) -> String {
        fs::read_to_string(self.jobs_file()).unwrap_or_default()
    }

    pub fn write_jobs_toml(&self, text: &str) {
        fs::create_dir_all(self.jobs_file().parent().unwrap()).unwrap();
        fs::write(self.jobs_file(), text).unwrap();
    }

    pub fn plist(&self, job: &str) -> Option<String> {
        fs::read_to_string(self.agents_dir().join(format!("{LABEL_PREFIX}{job}.plist"))).ok()
    }

    /// The labels the stand-in `launchctl` has loaded, sorted.
    pub fn loaded(&self) -> Vec<String> {
        let dir = self.root.path().join("launchctl/loaded");
        let mut labels: Vec<String> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        labels.sort();
        labels
    }
}

impl Sandbox {
    /// The messages of the notifications posted, in order.
    pub fn notifications(&self) -> Vec<String> {
        fs::read_to_string(self.root.path().join("notifications"))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

pub fn label(job: &str) -> String {
    format!("{LABEL_PREFIX}{job}")
}
