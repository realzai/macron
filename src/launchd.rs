//! launchd: the plists macron writes, and the `launchctl` calls that load them.
//!
//! macron owns every agent whose label starts with [`LABEL_PREFIX`] and touches nothing else in
//! `~/Library/LaunchAgents`. `launchctl` is judged only by its exit status, because its output is
//! undocumented and changes between macOS releases.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use plist::{Dictionary, Value};
use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::job::Job;
use crate::paths::Paths;
use crate::schedule::LaunchdTrigger;

/// Every launchd label macron creates starts with this, followed by the job's name.
pub const LABEL_PREFIX: &str = "io.github.realzai.macron.";

/// `launchctl print` exits with this when the label is not loaded.
const PRINT_NOT_LOADED: i32 = 113;

/// `launchctl bootout` exits with this ("No such process") when the label is not loaded.
const BOOTOUT_NOT_LOADED: i32 = 3;

pub fn label(job: &str) -> String {
    format!("{LABEL_PREFIX}{job}")
}

pub fn plist_path(paths: &Paths, job: &str) -> PathBuf {
    paths
        .launch_agents_dir
        .join(format!("{}.plist", label(job)))
}

/// The file launchd writes the job's process output to.
pub fn launchd_log(paths: &Paths, job: &str) -> PathBuf {
    paths.launchd_logs_dir.join(format!("{job}.log"))
}

/// The plist that has launchd run `job` on its schedule, by running `<binary> exec <job>`.
///
/// It sets `MACRON_CONFIG_DIR` and `MACRON_STATE_DIR`, so that `exec` reads and writes the same
/// files as the macron that wrote the plist, whatever launchd's environment holds.
pub fn render(job: &Job, binary: &Path, paths: &Paths) -> Vec<u8> {
    // APFS requires file names to be valid UTF-8, so these conversions lose nothing.
    let path = |path: &Path| Value::String(path.to_string_lossy().into_owned());
    let text = |text: &str| Value::String(text.to_owned());

    let mut agent = Dictionary::new();
    agent.insert("Label".to_owned(), Value::String(label(job.name())));
    agent.insert(
        "ProgramArguments".to_owned(),
        Value::Array(vec![path(binary), text("exec"), text(job.name())]),
    );
    let mut env = Dictionary::new();
    env.insert("MACRON_CONFIG_DIR".to_owned(), path(&paths.config_dir));
    env.insert("MACRON_STATE_DIR".to_owned(), path(&paths.state_dir));
    agent.insert("EnvironmentVariables".to_owned(), Value::Dictionary(env));
    match job.schedule().launchd_trigger() {
        LaunchdTrigger::Calendar(intervals) => {
            let entries = intervals
                .iter()
                .map(|interval| {
                    let mut entry = Dictionary::new();
                    for (key, value) in [
                        ("Month", interval.month),
                        ("Day", interval.day),
                        ("Weekday", interval.weekday),
                        ("Hour", interval.hour),
                        ("Minute", interval.minute),
                    ] {
                        if let Some(value) = value {
                            entry.insert(key.to_owned(), Value::Integer(u64::from(value).into()));
                        }
                    }
                    Value::Dictionary(entry)
                })
                .collect();
            agent.insert("StartCalendarInterval".to_owned(), Value::Array(entries));
        }
        LaunchdTrigger::Interval(secs) => {
            agent.insert("StartInterval".to_owned(), Value::Integer(secs.into()));
        }
    }
    let log = path(&launchd_log(paths, job.name()));
    agent.insert("StandardOutPath".to_owned(), log.clone());
    agent.insert("StandardErrorPath".to_owned(), log);

    let mut xml = Vec::new();
    Value::Dictionary(agent)
        .to_writer_xml(&mut xml)
        .expect("writing a plist of strings and integers to memory cannot fail");
    xml.push(b'\n');
    xml
}

/// The path to run macron from: the one this process was started as. A symlink is kept as the
/// symlink, so a Homebrew upgrade, which moves the real file but keeps the link, leaves installed
/// jobs working.
pub fn current_binary() -> Result<PathBuf> {
    std::env::current_exe().map_err(Error::io("locate", "the running macron binary"))
}

/// Where a job stands with launchd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Installed, up to date and loaded: launchd runs it on schedule.
    Active,
    /// Enabled, but not installed.
    NotInstalled,
    /// Installed, but its plist no longer matches the job.
    OutOfDate,
    /// Installed and up to date, but launchd has not loaded it.
    NotLoaded,
    /// Disabled and uninstalled.
    Disabled,
    /// Disabled, but still installed.
    DisabledButInstalled,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Active => "active",
            State::NotInstalled => "not installed",
            State::OutOfDate => "out of date",
            State::NotLoaded => "not loaded",
            State::Disabled => "disabled",
            State::DisabledButInstalled => "disabled, still installed",
        }
    }

    /// Whether the job is where its config says it should be.
    pub fn is_settled(self) -> bool {
        matches!(self, State::Active | State::Disabled)
    }
}

/// `job`'s state, given the changes [`plan`] found.
pub fn state(job: &Job, changes: &[Change]) -> State {
    let pending = changes
        .iter()
        .find(|change| change.job == job.name())
        .map(|change| change.action);
    match (job.enabled(), pending) {
        (_, Some(Action::Remove)) => State::DisabledButInstalled,
        (false, _) => State::Disabled,
        (true, None) => State::Active,
        (true, Some(Action::Install)) => State::NotInstalled,
        (true, Some(Action::Update)) => State::OutOfDate,
        (true, Some(Action::Load)) => State::NotLoaded,
    }
}

/// What [`apply`] does to one job's launchd agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Write the plist and load it: a new job, or one newly enabled.
    Install,
    /// Rewrite the plist and reload it: its schedule, the macron binary or a path changed.
    Update,
    /// Load a plist that is already right but is not loaded.
    Load,
    /// Unload the agent and delete its plist: the job was disabled or removed.
    Remove,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Install => "install",
            Action::Update => "update",
            Action::Load => "load",
            Action::Remove => "remove",
        }
    }
}

/// One change [`apply`] makes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub action: Action,
    pub job: String,
    pub label: String,
    /// The plist to write, for installs and updates.
    #[serde(skip)]
    plist: Option<Vec<u8>>,
}

impl Change {
    fn new(action: Action, job: &str, plist: Option<Vec<u8>>) -> Self {
        Self {
            action,
            job: job.to_owned(),
            label: label(job),
            plist,
        }
    }
}

/// The changes that make launchd run exactly the enabled jobs in `config`, as `<binary> exec`.
pub fn plan(
    config: &Config,
    binary: &Path,
    paths: &Paths,
    launchctl: &Launchctl,
) -> Result<Vec<Change>> {
    let wanted = config
        .jobs()
        .filter(|job| job.enabled())
        .map(|job| (job.name().to_owned(), render(job, binary, paths)))
        .collect();
    let installed = installed(&paths.launch_agents_dir)?;
    diff(&wanted, &installed, |job| launchctl.is_loaded(&label(job)))
}

/// Makes the changes, in order. On failure the changes before it stay made; running [`plan`] again
/// picks up from where it stopped.
pub fn apply(changes: &[Change], paths: &Paths, launchctl: &Launchctl) -> Result<()> {
    for change in changes {
        let plist = plist_path(paths, &change.job);
        if change.action != Action::Load {
            launchctl.unload(&change.label)?;
        }
        if let Some(contents) = &change.plist {
            create_dir(&paths.launch_agents_dir)?;
            fs::write(&plist, contents).map_err(Error::io("write", &plist))?;
        }
        match change.action {
            Action::Install | Action::Update | Action::Load => {
                create_dir(&paths.launchd_logs_dir)?;
                launchctl.load(&plist)?;
            }
            Action::Remove => match fs::remove_file(&plist) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => {
                    return Err(Error::io("delete", &plist)(err));
                }
                _ => {}
            },
        }
    }
    Ok(())
}

/// The program each plist macron has installed runs, by job name: what launchd starts for it.
/// `None` for a plist that is not in the shape macron writes.
pub fn installed_programs(dir: &Path) -> Result<BTreeMap<String, Option<PathBuf>>> {
    let programs = installed(dir)?
        .into_iter()
        .map(|(job, plist)| (job, program(&plist)))
        .collect();
    Ok(programs)
}

fn program(plist: &[u8]) -> Option<PathBuf> {
    let value = Value::from_reader(io::Cursor::new(plist)).ok()?;
    let arguments = value.as_dictionary()?.get("ProgramArguments")?.as_array()?;
    arguments.first()?.as_string().map(PathBuf::from)
}

/// The plists macron has installed, by job name.
fn installed(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => return Err(Error::io("read", dir)(err)),
    };
    let mut installed = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(Error::io("read", dir))?;
        let file_name = entry.file_name();
        let Some(job) = file_name
            .to_str()
            .and_then(|name| name.strip_prefix(LABEL_PREFIX))
            .and_then(|name| name.strip_suffix(".plist"))
        else {
            continue;
        };
        let path = entry.path();
        let contents = fs::read(&path).map_err(Error::io("read", &path))?;
        installed.insert(job.to_owned(), contents);
    }
    Ok(installed)
}

fn diff(
    wanted: &BTreeMap<String, Vec<u8>>,
    installed: &BTreeMap<String, Vec<u8>>,
    is_loaded: impl Fn(&str) -> Result<bool>,
) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    for (job, plist) in wanted {
        let action = match installed.get(job) {
            None => Action::Install,
            Some(current) if current != plist => Action::Update,
            Some(_) if !is_loaded(job)? => Action::Load,
            Some(_) => continue,
        };
        let plist = (action != Action::Load).then(|| plist.clone());
        changes.push(Change::new(action, job, plist));
    }
    for job in installed.keys().filter(|job| !wanted.contains_key(*job)) {
        changes.push(Change::new(Action::Remove, job, None));
    }
    changes.sort_by(|a, b| a.job.cmp(&b.job));
    Ok(changes)
}

fn create_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(Error::io("create", dir))
}

/// `launchctl`, aimed at the logged-in user's GUI domain, where per-user agents live.
#[derive(Debug, Clone)]
pub struct Launchctl {
    program: PathBuf,
    domain: String,
}

impl Launchctl {
    /// The real `launchctl`, or the program `MACRON_LAUNCHCTL` names, which is how tests run
    /// without touching launchd.
    pub fn from_env() -> Self {
        let program = std::env::var_os("MACRON_LAUNCHCTL")
            .filter(|program| !program.is_empty())
            .map_or_else(|| PathBuf::from("/bin/launchctl"), PathBuf::from);
        Self {
            program,
            domain: format!("gui/{}", nix::unistd::getuid()),
        }
    }

    pub fn is_loaded(&self, label: &str) -> Result<bool> {
        let target = format!("{}/{label}", self.domain);
        let status = self.run(&["print", &target], &[PRINT_NOT_LOADED])?;
        Ok(status == 0)
    }

    pub fn load(&self, plist: &Path) -> Result<()> {
        let plist = plist.to_string_lossy();
        self.run(&["bootstrap", &self.domain, &plist], &[])
            .map(drop)
    }

    /// Has launchd start the label's job now, as if its time had come.
    pub fn start(&self, label: &str) -> Result<()> {
        let target = format!("{}/{label}", self.domain);
        self.run(&["kickstart", &target], &[]).map(drop)
    }

    /// Unloads the label. One that is not loaded is left as it is.
    pub fn unload(&self, label: &str) -> Result<()> {
        let target = format!("{}/{label}", self.domain);
        self.run(&["bootout", &target], &[BOOTOUT_NOT_LOADED])
            .map(drop)
    }

    /// Runs `launchctl` with `args`, returning its exit status if it is 0 or one of `expected`.
    fn run(&self, args: &[&str], expected: &[i32]) -> Result<i32> {
        let output = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(Error::io("run", &self.program))?;
        let status = output.status.code().unwrap_or(-1);
        if status == 0 || expected.contains(&status) {
            return Ok(status);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        Err(Error::Launchctl {
            command: format!("launchctl {}", args.join(" ")),
            status,
            detail: if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use tempfile::TempDir;

    use super::*;
    use crate::job::JobSpec;

    fn paths(root: &Path) -> Paths {
        Paths::resolve(|key| match key {
            "HOME" => Some(OsString::from("/Users/ada")),
            "MACRON_CONFIG_DIR" => Some(root.join("config").into()),
            "MACRON_STATE_DIR" => Some(root.join("state").into()),
            "MACRON_LAUNCH_AGENTS_DIR" => Some(root.join("agents").into()),
            _ => None,
        })
        .unwrap()
    }

    fn job(name: &str, schedule: &str) -> Job {
        let spec = JobSpec {
            schedule: schedule.to_owned(),
            command: "true".to_owned(),
            ..JobSpec::default()
        };
        Job::new(name, spec).unwrap()
    }

    fn rendered(schedule: &str) -> String {
        let paths = paths(Path::new("/m"));
        let plist = render(&job("backup", schedule), Path::new("/bin/macron"), &paths);
        String::from_utf8(plist).unwrap()
    }

    const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>io.github.realzai.macron.backup</string>
	<key>ProgramArguments</key>
	<array>
		<string>/bin/macron</string>
		<string>exec</string>
		<string>backup</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>MACRON_CONFIG_DIR</key>
		<string>/m/config</string>
		<key>MACRON_STATE_DIR</key>
		<string>/m/state</string>
	</dict>
"#;

    const TAIL: &str = r#"	<key>StandardOutPath</key>
	<string>/m/state/launchd/backup.log</string>
	<key>StandardErrorPath</key>
	<string>/m/state/launchd/backup.log</string>
</dict>
</plist>
"#;

    #[test]
    fn renders_a_calendar_schedule() {
        let expected = r#"	<key>StartCalendarInterval</key>
	<array>
		<dict>
			<key>Day</key>
			<integer>13</integer>
			<key>Hour</key>
			<integer>9</integer>
			<key>Minute</key>
			<integer>30</integer>
		</dict>
		<dict>
			<key>Weekday</key>
			<integer>5</integer>
			<key>Hour</key>
			<integer>9</integer>
			<key>Minute</key>
			<integer>30</integer>
		</dict>
	</array>
"#;
        assert_eq!(rendered("30 9 13 * FRI"), format!("{HEAD}{expected}{TAIL}"));
    }

    #[test]
    fn renders_an_interval() {
        let expected = "\t<key>StartInterval</key>\n\t<integer>900</integer>\n";
        assert_eq!(rendered("@every 15m"), format!("{HEAD}{expected}{TAIL}"));
    }

    #[test]
    fn renders_every_minute_as_one_empty_entry() {
        let expected = "\t<key>StartCalendarInterval</key>\n\t<array>\n\t\t<dict/>\n\t</array>\n";
        assert_eq!(rendered("* * * * *"), format!("{HEAD}{expected}{TAIL}"));
    }

    #[test]
    fn reads_back_the_program_a_plist_runs() {
        let plist = rendered("@daily");
        assert_eq!(
            program(plist.as_bytes()),
            Some(PathBuf::from("/bin/macron"))
        );
        assert_eq!(program(b"not a plist"), None);
    }

    #[test]
    fn finds_only_its_own_plists() {
        let dir = TempDir::new().unwrap();
        for name in [
            "io.github.realzai.macron.backup.plist",
            "io.github.realzai.macron.sweep.plist",
            "com.example.other.plist",
            "io.github.realzai.macron.notes.txt",
        ] {
            fs::write(dir.path().join(name), name).unwrap();
        }
        let found = installed(dir.path()).unwrap();
        let jobs: Vec<_> = found.keys().map(String::as_str).collect();
        assert_eq!(jobs, ["backup", "sweep"]);
        assert!(installed(&dir.path().join("missing")).unwrap().is_empty());
    }

    fn plists(entries: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        entries
            .iter()
            .map(|(job, plist)| (job.to_string(), plist.as_bytes().to_vec()))
            .collect()
    }

    fn actions(changes: Vec<Change>) -> Vec<(Action, String)> {
        changes
            .into_iter()
            .map(|change| (change.action, change.job))
            .collect()
    }

    #[test]
    fn plans_installs_updates_loads_and_removals() {
        let wanted = plists(&[
            ("new", "a"),
            ("changed", "b2"),
            ("unloaded", "c"),
            ("same", "d"),
        ]);
        let installed = plists(&[
            ("changed", "b1"),
            ("unloaded", "c"),
            ("same", "d"),
            ("gone", "e"),
        ]);
        let changes = diff(&wanted, &installed, |job| Ok(job != "unloaded")).unwrap();
        assert_eq!(
            actions(changes),
            [
                (Action::Update, "changed".to_owned()),
                (Action::Remove, "gone".to_owned()),
                (Action::Install, "new".to_owned()),
                (Action::Load, "unloaded".to_owned()),
            ]
        );
    }

    #[test]
    fn plans_nothing_when_launchd_matches() {
        let both = plists(&[("same", "d")]);
        assert!(diff(&both, &both, |_| Ok(true)).unwrap().is_empty());
    }
}
