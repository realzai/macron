//! Rendering: JSON for scripts and agents, aligned text for people.

use std::io::{self, IsTerminal};
use std::path::Path;

use jiff::Zoned;
use serde::Serialize;

use crate::duration::Duration;
use crate::error::Result;
use crate::history::{Run, Status};

/// Prints `value` as one pretty-printed JSON document on stdout.
pub fn json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Prints rows as columns two spaces apart, each as wide as its widest cell. The last column is not
/// padded, so no line ends in spaces.
pub fn table(header: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = header.iter().map(|cell| cell.chars().count()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let header: Vec<String> = header.iter().map(|cell| (*cell).to_owned()).collect();
    for row in std::iter::once(&header).chain(rows) {
        let mut line = String::new();
        for (index, (cell, width)) in row.iter().zip(&widths).enumerate() {
            if index + 1 == row.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  "));
            }
        }
        println!("{}", line.trim_end());
    }
}

/// Prints `label value` pairs with the values lined up, indented under a heading.
pub fn fields(fields: &[(&str, String)]) {
    let width = fields
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    for (label, value) in fields {
        println!("  {label:<width$}  {value}");
    }
}

/// A time for people: `Sun 1 Nov 09:00`, with the year when it is not this year's.
pub fn human_time(time: &Zoned, now: &Zoned) -> String {
    let format = if time.year() == now.year() {
        "%a %-d %b %H:%M"
    } else {
        "%a %-d %b %Y %H:%M"
    };
    time.strftime(format).to_string()
}

/// How long ago `time` was: `just now`, `5m ago`, `3h ago`, `2d ago`, or past a week, the date.
pub fn ago(time: &Zoned, now: &Zoned) -> String {
    let secs = now.timestamp().as_second() - time.timestamp().as_second();
    match secs {
        ..60 => "just now".to_owned(),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        86_400..604_800 => format!("{}d ago", secs / 86_400),
        _ if time.year() == now.year() => time.strftime("%-d %b").to_string(),
        _ => time.strftime("%-d %b %Y").to_string(),
    }
}

/// How long something took: `<1s`, `45s`, `3m 2s`.
pub fn took(milliseconds: i64) -> String {
    match u64::try_from(milliseconds / 1_000) {
        Ok(0) | Err(_) => "<1s".to_owned(),
        Ok(secs) => Duration::from_secs(secs).to_string(),
    }
}

/// A run's status in a few words: `succeeded`, `failed, exit 3`, `killed, SIGKILL`, `timed out`.
pub fn run_status(run: &Run) -> String {
    match (run.status, &run.error, run.exit_code, run.signal) {
        (Status::Running, ..) => "running".to_owned(),
        (Status::Succeeded, ..) => "succeeded".to_owned(),
        (Status::TimedOut, ..) => "timed out".to_owned(),
        (Status::Lost, ..) => "lost".to_owned(),
        (Status::Failed, Some(_), ..) => "could not start".to_owned(),
        (Status::Failed, None, Some(code), _) => format!("failed, exit {code}"),
        (Status::Failed, None, None, Some(signal)) => format!("killed, {}", signal_name(signal)),
        (Status::Failed, None, None, None) => "failed".to_owned(),
    }
}

/// `SIGKILL` for 9, or `signal 99` for a number macOS does not name.
pub fn signal_name(signal: i32) -> String {
    nix::sys::signal::Signal::try_from(signal).map_or_else(
        |_| format!("signal {signal}"),
        |signal| signal.as_str().to_owned(),
    )
}

/// Colour for a terminal: none when stdout is not one, or when `NO_COLOR` is set.
#[derive(Debug, Clone, Copy)]
pub struct Paint {
    enabled: bool,
}

impl Paint {
    pub fn stdout() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        Self {
            enabled: io::stdout().is_terminal() && !no_color,
        }
    }

    pub fn red(self, text: &str) -> String {
        self.paint("31", text)
    }

    pub fn dim(self, text: &str) -> String {
        self.paint("2", text)
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}

/// `path` with the home directory written as `~`.
pub fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_times_for_people_and_for_json() {
        let now: Zoned = "2026-10-10T12:00:00+01:00[Europe/London]".parse().unwrap();
        let soon: Zoned = "2026-11-01T09:05:00+00:00[Europe/London]".parse().unwrap();
        let later: Zoned = "2027-01-01T09:00:00+00:00[Europe/London]".parse().unwrap();
        assert_eq!(human_time(&soon, &now), "Sun 1 Nov 09:05");
        assert_eq!(human_time(&later, &now), "Fri 1 Jan 2027 09:00");
    }

    #[test]
    fn says_how_long_ago() {
        let now: Zoned = "2026-10-10T12:00:00+01:00[Europe/London]".parse().unwrap();
        let before = |text: &str| -> Zoned { text.parse().unwrap() };
        assert_eq!(
            ago(&before("2026-10-10T11:59:30+01:00[Europe/London]"), &now),
            "just now"
        );
        assert_eq!(
            ago(&before("2026-10-10T11:55:00+01:00[Europe/London]"), &now),
            "5m ago"
        );
        assert_eq!(
            ago(&before("2026-10-10T09:00:00+01:00[Europe/London]"), &now),
            "3h ago"
        );
        assert_eq!(
            ago(&before("2026-10-08T12:00:00+01:00[Europe/London]"), &now),
            "2d ago"
        );
        assert_eq!(
            ago(&before("2026-09-01T12:00:00+01:00[Europe/London]"), &now),
            "1 Sep"
        );
        assert_eq!(
            ago(&before("2025-09-01T12:00:00+01:00[Europe/London]"), &now),
            "1 Sep 2025"
        );
    }

    #[test]
    fn says_how_long_something_took() {
        assert_eq!(took(400), "<1s");
        assert_eq!(took(45_900), "45s");
        assert_eq!(took(182_000), "3m 2s");
    }

    #[test]
    fn abbreviates_home() {
        let home = Path::new("/Users/ada");
        assert_eq!(tilde(Path::new("/Users/ada"), home), "~");
        assert_eq!(tilde(Path::new("/Users/ada/x/y"), home), "~/x/y");
        assert_eq!(tilde(Path::new("/tmp/x"), home), "/tmp/x");
    }
}
