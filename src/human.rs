//! Text for people: times, durations and run statuses, as the CLI and the web UI both show them.

use std::path::Path;

use jiff::Zoned;

use crate::duration::Duration;
use crate::history::{Run, Status};

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

/// How a run went, as a sentence: "Succeeded in 2s.", "Failed with exit status 3 after 2s."
pub fn summary(run: &Run) -> String {
    let took = run.duration_ms.map(took).unwrap_or_default();
    match (run.status, &run.error, run.exit_code, run.signal) {
        (Status::Running, ..) => "Still running.".to_owned(),
        (Status::Succeeded, ..) => format!("Succeeded in {took}."),
        (Status::TimedOut, ..) => format!("Stopped at its timeout after {took}."),
        (Status::Lost, error, ..) | (Status::Failed, error @ Some(_), ..) => {
            let reason = error.as_deref().unwrap_or("no reason recorded");
            let what = if run.status == Status::Lost {
                "Lost"
            } else {
                "Could not start"
            };
            format!("{what}: {reason}.")
        }
        (Status::Failed, None, Some(code), _) => {
            format!("Failed with exit status {code} after {took}.")
        }
        (Status::Failed, None, None, Some(signal)) => {
            format!("Killed by {} after {took}.", signal_name(signal))
        }
        (Status::Failed, None, None, None) => format!("Failed after {took}."),
    }
}

/// `SIGKILL` for 9, or `signal 99` for a number macOS does not name.
pub fn signal_name(signal: i32) -> String {
    nix::sys::signal::Signal::try_from(signal).map_or_else(
        |_| format!("signal {signal}"),
        |signal| signal.as_str().to_owned(),
    )
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
