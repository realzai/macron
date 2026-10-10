//! `jobs.toml`: reading it, and editing it without losing what people wrote in it.
//!
//! Reading needs no lock. A save replaces the file in one rename, so a reader sees the old file or
//! the new one, never half of each. Editing holds an exclusive lock across the whole
//! read-modify-write, so the CLI and the web UI cannot overwrite each other's changes.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::error::{Error, Result};
use crate::job::{Job, JobSpec};
use crate::paths::Paths;

/// The jobs in `jobs.toml`, validated, in name order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    jobs: BTreeMap<String, Job>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    jobs: BTreeMap<String, JobSpec>,
}

impl Config {
    /// Reads and validates the file at `path`. A file that does not exist is an empty config.
    pub fn load(path: &Path) -> Result<Self> {
        let text = read(path)?;
        Self::parse(&text).map_err(|reason| Error::InvalidConfig {
            path: path.to_owned(),
            reason,
        })
    }

    fn parse(text: &str) -> Result<Self, String> {
        let file: ConfigFile =
            toml::from_str(text).map_err(|err| err.to_string().trim_end().to_owned())?;
        let mut jobs = BTreeMap::new();
        for (name, spec) in file.jobs {
            let job = Job::new(&name, spec).map_err(|err| match err {
                Error::InvalidJob { .. } | Error::InvalidJobName(_) => err.to_string(),
                other => format!("job `{name}`: {other}"),
            })?;
            jobs.insert(name, job);
        }
        Ok(Self { jobs })
    }

    pub fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.values()
    }

    pub fn job(&self, name: &str) -> Result<&Job> {
        self.jobs
            .get(name)
            .ok_or_else(|| Error::NoSuchJob(name.to_owned()))
    }
}

/// `jobs.toml` open for editing.
///
/// It holds an exclusive lock, so other macron processes wait to edit until it is saved or dropped.
/// Edits change the document as written, keeping comments, formatting and the order of everything
/// they do not touch.
pub struct ConfigEditor {
    path: PathBuf,
    document: DocumentMut,
    _lock: File,
}

impl ConfigEditor {
    pub fn open(paths: &Paths) -> Result<Self> {
        let lock = lock(&paths.state_dir.join("jobs.lock"))?;
        let path = paths.jobs_file.clone();
        let text = read(&path)?;
        let invalid = |reason: String| Error::InvalidConfig {
            path: path.clone(),
            reason,
        };
        Config::parse(&text).map_err(invalid)?;
        let document = text
            .parse::<DocumentMut>()
            .map_err(|err| invalid(err.to_string()))?;
        Ok(Self {
            path,
            document,
            _lock: lock,
        })
    }

    /// The config with the edits made so far.
    pub fn config(&self) -> Result<Config> {
        Config::parse(&self.document.to_string()).map_err(|reason| Error::InvalidConfig {
            path: self.path.clone(),
            reason,
        })
    }

    /// Adds a job, which must be valid and must not exist yet.
    pub fn insert(&mut self, name: &str, spec: &JobSpec) -> Result<()> {
        Job::new(name, spec.clone())?;
        let jobs = self.jobs_table();
        if jobs.contains_key(name) {
            return Err(Error::JobExists(name.to_owned()));
        }
        let mut table = Table::new();
        write_spec(&mut table, spec);
        jobs.insert(name, Item::Table(table));
        Ok(())
    }

    /// Replaces a job's fields with `spec`'s, keeping the comments and formatting of values that
    /// stay the same, and of the lines whose values change.
    pub fn update(&mut self, name: &str, spec: &JobSpec) -> Result<()> {
        Job::new(name, spec.clone())?;
        let table = self
            .jobs_table()
            .get_mut(name)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| Error::NoSuchJob(name.to_owned()))?;
        write_spec(table, spec);
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        match self.jobs_table().remove(name) {
            Some(_) => Ok(()),
            None => Err(Error::NoSuchJob(name.to_owned())),
        }
    }

    /// Writes the edits, if the file as a whole is still valid, and returns the config they make.
    pub fn save(self) -> Result<Config> {
        let text = self.document.to_string();
        let config = Config::parse(&text).map_err(|reason| Error::InvalidConfig {
            path: self.path.clone(),
            reason,
        })?;
        write_atomically(&self.path, &text)?;
        Ok(config)
    }

    /// The `[jobs]` table, created if missing and converted to a standard table if it was written
    /// inline, so jobs can be added to it as `[jobs.<name>]` sections.
    fn jobs_table(&mut self) -> &mut Table {
        let jobs = self
            .document
            .as_table_mut()
            .entry("jobs")
            .or_insert_with(|| {
                let mut jobs = Table::new();
                jobs.set_implicit(true);
                Item::Table(jobs)
            });
        if let Item::Value(Value::InlineTable(inline)) = jobs {
            *jobs = Item::Table(std::mem::take(inline).into_table());
        }
        jobs.as_table_mut()
            .expect("`jobs` is a table: Config::parse checked it")
    }
}

fn write_spec(table: &mut dyn TableLike, spec: &JobSpec) {
    let text = |text: &Option<String>| text.as_deref().map(Value::from);
    set(table, "schedule", Some(Value::from(spec.schedule.as_str())));
    set(table, "command", Some(Value::from(spec.command.as_str())));
    set(table, "description", text(&spec.description));
    set(table, "workdir", text(&spec.workdir));
    set(table, "shell", text(&spec.shell));
    set(
        table,
        "env",
        (!spec.env.is_empty()).then(|| {
            Value::InlineTable(
                spec.env
                    .iter()
                    .map(|(key, value)| (key.as_str(), Value::from(value.as_str())))
                    .collect::<InlineTable>(),
            )
        }),
    );
    set(table, "timeout", text(&spec.timeout));
    set(
        table,
        "notify",
        spec.notify.map(|notify| Value::from(notify.as_str())),
    );
    set(
        table,
        "keep_runs",
        spec.keep_runs.map(|keep| Value::from(i64::from(keep))),
    );
    set(table, "enabled", spec.enabled.map(Value::from));
}

/// Sets `key` to `value`, or removes it for `None`. A value that does not change is left exactly as
/// written; one that does keeps the whitespace and comment around it.
fn set(table: &mut dyn TableLike, key: &str, value: Option<Value>) {
    let Some(mut value) = value else {
        table.remove(key);
        return;
    };
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(existing) if same(existing, &value) => {}
        Some(existing) => {
            *value.decor_mut() = existing.decor().clone();
            *existing = value;
        }
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::InlineTable(a), Value::InlineTable(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| same(value, other)))
        }
        _ => false,
    }
}

fn read(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(Error::io("read", path)(err)),
    }
}

fn lock(path: &Path) -> Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(Error::io("create", dir))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(Error::io("open", path))?;
    file.lock().map_err(Error::io("lock", path))?;
    Ok(file)
}

/// Replaces the file at `path` with `text` in one rename, so a reader never sees a partial file.
///
/// A symlink at `path`, as in a dotfiles setup, is followed and the file it points at replaced, so
/// the link survives. A replaced file keeps its permissions.
fn write_atomically(path: &Path, text: &str) -> Result<()> {
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(err) if err.kind() == io::ErrorKind::NotFound => path.to_owned(),
        Err(err) => return Err(Error::io("resolve", path)(err)),
    };
    let dir = target.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(dir).map_err(Error::io("create", dir))?;
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut file = File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        if let Ok(metadata) = fs::metadata(&target) {
            file.set_permissions(metadata.permissions())?;
        }
        file.sync_all()?;
        fs::rename(&temp, &target)
    })();
    written.map_err(|err| {
        let _ = fs::remove_file(&temp);
        Error::io("write", &target)(err)
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::TempDir;

    use super::*;
    use crate::job::Notify;

    struct Fixture {
        _dir: TempDir,
        paths: Paths,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let root = dir.path().to_owned();
            let paths = Paths::resolve(|key| match key {
                "HOME" => Some(OsString::from(&root)),
                "MACRON_CONFIG_DIR" => Some(root.join("config").into()),
                "MACRON_STATE_DIR" => Some(root.join("state").into()),
                _ => None,
            })
            .unwrap();
            Self { _dir: dir, paths }
        }

        fn with_file(text: &str) -> Self {
            let fixture = Self::new();
            fs::create_dir_all(&fixture.paths.config_dir).unwrap();
            fs::write(&fixture.paths.jobs_file, text).unwrap();
            fixture
        }

        fn file(&self) -> String {
            fs::read_to_string(&self.paths.jobs_file).unwrap()
        }

        fn edit(&self, change: impl FnOnce(&mut ConfigEditor) -> Result<()>) -> Result<Config> {
            let mut editor = ConfigEditor::open(&self.paths)?;
            change(&mut editor)?;
            editor.save()
        }

        fn load_error(&self) -> String {
            match Config::load(&self.paths.jobs_file) {
                Err(Error::InvalidConfig { reason, .. }) => reason,
                other => panic!("expected an invalid config, got {other:?}"),
            }
        }
    }

    fn spec(schedule: &str, command: &str) -> JobSpec {
        JobSpec {
            schedule: schedule.to_owned(),
            command: command.to_owned(),
            ..JobSpec::default()
        }
    }

    const COMMENTED: &str = r#"# My jobs.

[jobs.backup]
schedule = "0 9 1 * *"   # monthly
command = 'rsync -a ~/Photos /Volumes/Backup'
"#;

    #[test]
    fn a_missing_file_is_an_empty_config() {
        let fixture = Fixture::new();
        let config = Config::load(&fixture.paths.jobs_file).unwrap();
        assert_eq!(config.jobs().count(), 0);
    }

    #[test]
    fn loads_jobs_in_name_order() {
        let fixture = Fixture::with_file(
            r#"
            [jobs.zeta]
            schedule = "@daily"
            command = "true"

            [jobs.alpha]
            schedule = "@hourly"
            command = "true"
            notify = "always"
            "#,
        );
        let config = Config::load(&fixture.paths.jobs_file).unwrap();
        let names: Vec<_> = config.jobs().map(Job::name).collect();
        assert_eq!(names, ["alpha", "zeta"]);
        assert_eq!(config.job("alpha").unwrap().notify(), Notify::Always);
        assert!(matches!(config.job("beta"), Err(Error::NoSuchJob(_))));
    }

    #[test]
    fn rejects_unknown_fields_with_their_line() {
        let fixture =
            Fixture::with_file("[jobs.backup]\nschedule = \"@daily\"\ncomand = \"true\"\n");
        let reason = fixture.load_error();
        assert!(reason.contains("line 3"), "{reason}");
        assert!(reason.contains("unknown field `comand`"), "{reason}");
    }

    #[test]
    fn names_the_job_with_an_invalid_value() {
        let fixture =
            Fixture::with_file("[jobs.backup]\nschedule = \"0 25 * * *\"\ncommand = \"true\"\n");
        assert_eq!(
            fixture.load_error(),
            "job `backup`: invalid schedule \"0 25 * * *\": hour: `25` is out of range; use 0–23"
        );
        let fixture =
            Fixture::with_file("[jobs.Backup]\nschedule = \"@daily\"\ncommand = \"true\"\n");
        assert!(
            fixture
                .load_error()
                .starts_with("invalid job name \"Backup\"")
        );
    }

    #[test]
    fn inserting_into_a_new_file_writes_a_section_per_job() {
        let fixture = Fixture::new();
        fixture
            .edit(|editor| {
                editor.insert(
                    "backup",
                    &JobSpec {
                        env: [("MODE".to_owned(), "full".to_owned())].into(),
                        enabled: Some(false),
                        ..spec("@daily", "backup.sh")
                    },
                )
            })
            .unwrap();
        assert_eq!(
            fixture.file(),
            "[jobs.backup]\nschedule = \"@daily\"\ncommand = \"backup.sh\"\n\
             env = { MODE = \"full\" }\nenabled = false\n"
        );
    }

    #[test]
    fn inserting_keeps_what_is_already_written() {
        let fixture = Fixture::with_file(COMMENTED);
        fixture
            .edit(|editor| editor.insert("sweep", &spec("*/15 * * * *", "sweep")))
            .unwrap();
        assert_eq!(
            fixture.file(),
            format!(
                "{COMMENTED}\n[jobs.sweep]\nschedule = \"*/15 * * * *\"\ncommand = \"sweep\"\n"
            )
        );
    }

    #[test]
    fn updating_keeps_comments_and_unchanged_values_as_written() {
        let fixture = Fixture::with_file(COMMENTED);
        let config = fixture
            .edit(|editor| {
                let mut spec = editor.config()?.job("backup")?.spec().clone();
                spec.schedule = "0 10 1 * *".to_owned();
                spec.timeout = Some("2h".to_owned());
                editor.update("backup", &spec)
            })
            .unwrap();
        assert_eq!(
            fixture.file(),
            COMMENTED.replace("0 9 1 * *", "0 10 1 * *") + "timeout = \"2h\"\n"
        );
        assert_eq!(config.job("backup").unwrap().schedule_text(), "0 10 1 * *");
    }

    #[test]
    fn updating_removes_fields_set_back_to_their_default() {
        let fixture = Fixture::with_file(
            "[jobs.backup]\nschedule = \"@daily\"\ncommand = \"true\"\ntimeout = \"1h\"\n",
        );
        fixture
            .edit(|editor| editor.update("backup", &spec("@daily", "true")))
            .unwrap();
        assert_eq!(
            fixture.file(),
            "[jobs.backup]\nschedule = \"@daily\"\ncommand = \"true\"\n"
        );
    }

    #[test]
    fn removing_drops_the_job_and_its_comments() {
        let fixture = Fixture::with_file(COMMENTED);
        let config = fixture.edit(|editor| editor.remove("backup")).unwrap();
        assert_eq!(config.jobs().count(), 0);
        assert!(!fixture.file().contains("backup"));
    }

    #[test]
    fn refuses_duplicate_missing_and_invalid_jobs_without_writing() {
        let fixture = Fixture::with_file(COMMENTED);
        let refuse = |change: fn(&mut ConfigEditor) -> Result<()>| {
            let err = fixture.edit(change).unwrap_err();
            assert_eq!(fixture.file(), COMMENTED);
            err.code()
        };
        assert_eq!(
            refuse(|editor| editor.insert("backup", &spec("@daily", "true"))),
            "job_exists"
        );
        assert_eq!(
            refuse(|editor| editor.update("nope", &spec("@daily", "true"))),
            "no_such_job"
        );
        assert_eq!(refuse(|editor| editor.remove("nope")), "no_such_job");
        assert_eq!(
            refuse(|editor| editor.insert("new", &spec("@sometimes", "true"))),
            "invalid_schedule"
        );
        assert_eq!(
            refuse(|editor| editor.insert("New", &spec("@daily", "true"))),
            "invalid_job_name"
        );
    }

    #[test]
    fn refuses_to_edit_an_invalid_file() {
        let fixture = Fixture::with_file("[jobs.backup]\nschedule = \"@daily\"\n");
        let err = ConfigEditor::open(&fixture.paths).err().unwrap();
        assert_eq!(err.code(), "invalid_config");
    }

    #[test]
    fn saving_writes_through_a_symlink_and_keeps_permissions() {
        let fixture = Fixture::new();
        let dotfiles = fixture.paths.state_dir.parent().unwrap().join("dotfiles");
        fs::create_dir_all(&dotfiles).unwrap();
        fs::create_dir_all(&fixture.paths.config_dir).unwrap();
        let real = dotfiles.join("jobs.toml");
        fs::write(&real, "").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&real, &fixture.paths.jobs_file).unwrap();

        fixture
            .edit(|editor| editor.insert("backup", &spec("@daily", "true")))
            .unwrap();

        let link = fs::symlink_metadata(&fixture.paths.jobs_file).unwrap();
        assert!(link.file_type().is_symlink());
        assert!(fs::read_to_string(&real).unwrap().contains("[jobs.backup]"));
        let mode = fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn converts_an_inline_jobs_table_before_inserting() {
        let fixture = Fixture::with_file("jobs = {}\n");
        fixture
            .edit(|editor| editor.insert("backup", &spec("@daily", "true")))
            .unwrap();
        let config = Config::load(&fixture.paths.jobs_file).unwrap();
        assert_eq!(config.job("backup").unwrap().command(), "true");
    }
}
