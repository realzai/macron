//! The job form: what the browser sends, turned into a [`JobSpec`], and back again for editing.
//!
//! Empty fields mean "use the default", as a field left out of `jobs.toml` does. Errors are tied to
//! the field they are about, so the form can show each one next to its input.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::Error;
use crate::job::{JobSpec, Notify};

/// The job form as submitted. Every field is text, as HTML sends it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct JobForm {
    pub name: String,
    pub schedule: String,
    pub command: String,
    pub description: String,
    pub workdir: String,
    pub shell: String,
    /// One `KEY=VALUE` per line.
    pub env: String,
    pub timeout: String,
    /// `never`, `failure` or `always`, or empty for the default.
    pub notify: String,
    pub keep_runs: String,
    /// Present when the box is ticked. HTML sends nothing for an unticked box.
    pub enabled: Option<String>,
}

/// What is wrong with a form, by field. `None` is about the job as a whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FormErrors {
    pub fields: BTreeMap<&'static str, String>,
    pub general: Option<String>,
}

impl FormErrors {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.general.is_none()
    }

    /// The error for `field`, or an empty string, for templates.
    pub fn get(&self, field: &str) -> &str {
        self.fields.get(field).map_or("", String::as_str)
    }

    fn add(&mut self, field: &'static str, message: impl Into<String>) {
        self.fields.entry(field).or_insert_with(|| message.into());
    }

    /// Ties an error from creating or changing the job to the field it is about.
    pub fn from_error(err: &Error) -> Self {
        let mut errors = Self::default();
        let message = match err {
            Error::InvalidSchedule { reason, .. } => ("schedule", reason.clone()),
            Error::InvalidDuration { reason, .. } => ("timeout", reason.clone()),
            Error::InvalidJobName(_) => (
                "name",
                "use lowercase letters, digits, `-` and `_`, starting with a letter or digit, \
                 up to 64 characters"
                    .to_owned(),
            ),
            Error::JobExists(_) => ("name", "there is already a job with this name".to_owned()),
            Error::InvalidJob { reason, .. } => {
                let field = [
                    ("command", "command"),
                    ("shell", "shell"),
                    ("workdir", "workdir"),
                    ("keep_runs", "keep_runs"),
                    ("env", "env"),
                ]
                .into_iter()
                .find(|(word, _)| reason.contains(word))
                .map(|(_, field)| field);
                match field {
                    Some(field) => (field, reason.clone()),
                    None => {
                        errors.general = Some(err.to_string());
                        return errors;
                    }
                }
            }
            other => {
                errors.general = Some(other.to_string());
                return errors;
            }
        };
        errors.add(message.0, message.1);
        errors
    }
}

impl JobForm {
    /// The form filled in from a job as written, for editing it.
    pub fn from_spec(name: &str, spec: &JobSpec) -> Self {
        let text = |value: &Option<String>| value.clone().unwrap_or_default();
        Self {
            name: name.to_owned(),
            schedule: spec.schedule.clone(),
            command: spec.command.clone(),
            description: text(&spec.description),
            workdir: text(&spec.workdir),
            shell: text(&spec.shell),
            env: spec
                .env
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("\n"),
            timeout: text(&spec.timeout),
            notify: spec
                .notify
                .map(|notify| notify.as_str().to_owned())
                .unwrap_or_default(),
            keep_runs: spec
                .keep_runs
                .map(|keep| keep.to_string())
                .unwrap_or_default(),
            enabled: spec.enabled.unwrap_or(true).then(|| "on".to_owned()),
        }
    }

    /// A blank form for a new job: enabled, with nothing else set.
    pub fn blank() -> Self {
        Self {
            enabled: Some("on".to_owned()),
            ..Self::default()
        }
    }

    /// The job the form describes, or what is wrong with the fields that are not even the right
    /// shape. Whether the job as a whole is valid is for [`Job::new`](crate::job::Job::new) to say.
    pub fn to_spec(&self) -> Result<JobSpec, FormErrors> {
        let mut errors = FormErrors::default();
        let optional = |text: &str| {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        };

        let mut env = BTreeMap::new();
        for line in self
            .env
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            match line.split_once('=') {
                Some((key, value)) => {
                    env.insert(key.trim().to_owned(), value.to_owned());
                }
                None => errors.add("env", format!("`{line}` is not KEY=VALUE")),
            }
        }
        let notify = match self.notify.trim() {
            "" => None,
            "never" => Some(Notify::Never),
            "failure" => Some(Notify::Failure),
            "always" => Some(Notify::Always),
            other => {
                errors.add(
                    "notify",
                    format!("`{other}` is not never, failure or always"),
                );
                None
            }
        };
        let keep_runs = match self.keep_runs.trim() {
            "" => None,
            text => match text.parse::<u32>() {
                Ok(keep) => Some(keep),
                Err(_) => {
                    errors.add("keep_runs", "keep runs must be a whole number");
                    None
                }
            },
        };
        if self.name.trim().is_empty() {
            errors.add("name", "a job needs a name");
        }
        if self.schedule.trim().is_empty() {
            errors.add("schedule", "a job needs a schedule");
        }
        if self.command.trim().is_empty() {
            errors.add("command", "a job needs a command");
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(JobSpec {
            schedule: self.schedule.trim().to_owned(),
            // Browsers send line breaks in a textarea as CRLF.
            command: self.command.replace("\r\n", "\n").trim().to_owned(),
            description: optional(&self.description),
            workdir: optional(&self.workdir),
            shell: optional(&self.shell),
            env,
            timeout: optional(&self.timeout),
            notify,
            keep_runs,
            enabled: if self.enabled.is_some() {
                None
            } else {
                Some(false)
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> JobForm {
        JobForm {
            name: "backup".to_owned(),
            schedule: " 0 9 * * * ".to_owned(),
            command: "echo one\r\necho two".to_owned(),
            enabled: Some("on".to_owned()),
            ..JobForm::default()
        }
    }

    #[test]
    fn empty_fields_mean_defaults() {
        let spec = form().to_spec().unwrap();
        assert_eq!(spec.schedule, "0 9 * * *");
        assert_eq!(spec.command, "echo one\necho two");
        assert_eq!(spec.description, None);
        assert_eq!(spec.notify, None);
        assert_eq!(spec.keep_runs, None);
        assert_eq!(spec.enabled, None);
        assert!(spec.env.is_empty());
    }

    #[test]
    fn reads_every_field() {
        let spec = JobForm {
            description: "Backups".to_owned(),
            workdir: "~/src".to_owned(),
            shell: "/bin/bash".to_owned(),
            env: "A=1\r\n\r\n B = two=2\n".to_owned(),
            timeout: "30m".to_owned(),
            notify: "always".to_owned(),
            keep_runs: "7".to_owned(),
            enabled: None,
            ..form()
        }
        .to_spec()
        .unwrap();
        assert_eq!(spec.description.as_deref(), Some("Backups"));
        assert_eq!(spec.workdir.as_deref(), Some("~/src"));
        assert_eq!(spec.shell.as_deref(), Some("/bin/bash"));
        assert_eq!(spec.env.get("A").map(String::as_str), Some("1"));
        assert_eq!(spec.env.get("B").map(String::as_str), Some(" two=2"));
        assert_eq!(spec.timeout.as_deref(), Some("30m"));
        assert_eq!(spec.notify, Some(Notify::Always));
        assert_eq!(spec.keep_runs, Some(7));
        assert_eq!(spec.enabled, Some(false));
    }

    #[test]
    fn round_trips_through_a_spec() {
        let spec = JobForm {
            env: "A=1\nB=2".to_owned(),
            timeout: "30m".to_owned(),
            ..form()
        }
        .to_spec()
        .unwrap();
        assert_eq!(JobForm::from_spec("backup", &spec).to_spec().unwrap(), spec);
    }

    #[test]
    fn ties_shape_errors_to_their_fields() {
        let errors = JobForm {
            schedule: String::new(),
            env: "NOEQUALS".to_owned(),
            keep_runs: "many".to_owned(),
            notify: "sometimes".to_owned(),
            ..form()
        }
        .to_spec()
        .unwrap_err();
        assert_eq!(errors.get("schedule"), "a job needs a schedule");
        assert_eq!(errors.get("env"), "`NOEQUALS` is not KEY=VALUE");
        assert_eq!(errors.get("keep_runs"), "keep runs must be a whole number");
        assert_eq!(
            errors.get("notify"),
            "`sometimes` is not never, failure or always"
        );
    }

    #[test]
    fn ties_job_errors_to_their_fields() {
        let schedule = Error::InvalidSchedule {
            schedule: "x".to_owned(),
            reason: "it has 1 field".to_owned(),
        };
        assert_eq!(
            FormErrors::from_error(&schedule).get("schedule"),
            "it has 1 field"
        );
        let shell = Error::InvalidJob {
            job: "a".to_owned(),
            reason: "its shell must be an absolute path, such as /bin/zsh".to_owned(),
        };
        assert!(
            FormErrors::from_error(&shell)
                .get("shell")
                .starts_with("its shell")
        );
        let exists = Error::JobExists("a".to_owned());
        assert_eq!(
            FormErrors::from_error(&exists).get("name"),
            "there is already a job with this name"
        );
        let other = Error::NoHome;
        assert!(FormErrors::from_error(&other).general.is_some());
    }
}
