//! Lengths of time as people write them: `90s`, `15m`, `1h 30m`, `2d`, `1w`.

use std::fmt;
use std::str::FromStr;

use jiff::{Span, SpanRelativeTo};

use crate::error::Error;

/// A whole number of seconds, used for `@every` schedules and job timeouts.
///
/// Parsed from jiff's friendly format (`1h 30m`, `90 seconds`) or ISO 8601 (`PT15M`). Days and weeks
/// count as 24 and 168 hours. Months and years are refused, because they have no fixed length.
/// Parsing also refuses zero, negative and fractional-second values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Duration {
    secs: u64,
}

impl Duration {
    pub const fn from_secs(secs: u64) -> Self {
        Self { secs }
    }

    pub const fn as_secs(self) -> u64 {
        self.secs
    }

    pub const fn as_std(self) -> std::time::Duration {
        std::time::Duration::from_secs(self.secs)
    }

    /// Parses `input`, returning why it is invalid rather than a full [`Error`], so that callers
    /// parsing something larger (such as `@every 15m`) can report it in their own terms.
    pub(crate) fn parse(input: &str) -> Result<Self, String> {
        let span: Span = input
            .trim()
            .parse()
            .map_err(|_| "expected a duration such as `90s`, `15m`, `1h 30m` or `2d`".to_owned())?;
        if span.get_years() != 0 || span.get_months() != 0 {
            return Err("months and years have no fixed length; use days or weeks".to_owned());
        }
        let duration = span
            .to_duration(SpanRelativeTo::days_are_24_hours())
            .map_err(|_| "it is too long".to_owned())?;
        if duration.subsec_nanos() != 0 {
            return Err("it must be a whole number of seconds".to_owned());
        }
        match u64::try_from(duration.as_secs()) {
            Ok(secs) if secs > 0 => Ok(Self { secs }),
            _ => Err("it must be longer than zero".to_owned()),
        }
    }
}

impl FromStr for Duration {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Error> {
        Self::parse(input).map_err(|reason| Error::InvalidDuration {
            value: input.to_owned(),
            reason,
        })
    }
}

/// Writes the duration in days, hours, minutes and seconds, largest first, leaving out units that
/// are zero: `1h 30m`, `2d`, `45s`. The output parses back to the same duration.
impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.secs == 0 {
            return f.write_str("0s");
        }
        let mut rest = self.secs;
        let mut separator = "";
        for (unit, size) in [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
            let count = rest / size;
            rest %= size;
            if count > 0 {
                write!(f, "{separator}{count}{unit}")?;
                separator = " ";
            }
        }
        Ok(())
    }
}

impl serde::Serialize for Duration {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Duration {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(|reason| {
            serde::de::Error::custom(format_args!("invalid duration {text:?}: {reason}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(input: &str) -> Result<u64, String> {
        Duration::parse(input).map(Duration::as_secs)
    }

    #[test]
    fn parses_the_ways_people_write_durations() {
        assert_eq!(secs("90s"), Ok(90));
        assert_eq!(secs("15m"), Ok(900));
        assert_eq!(secs("1h 30m"), Ok(5_400));
        assert_eq!(secs("1h30m"), Ok(5_400));
        assert_eq!(secs("2 hours"), Ok(7_200));
        assert_eq!(secs("1.5h"), Ok(5_400));
        assert_eq!(secs("2d"), Ok(172_800));
        assert_eq!(secs("1w"), Ok(604_800));
        assert_eq!(secs("PT15M"), Ok(900));
        assert_eq!(secs("  5m "), Ok(300));
    }

    #[test]
    fn refuses_what_is_not_a_fixed_positive_whole_number_of_seconds() {
        for (input, reason) in [
            ("", "expected a duration"),
            ("soon", "expected a duration"),
            ("15", "expected a duration"),
            ("1mo", "no fixed length"),
            ("1y", "no fixed length"),
            ("1.5s", "whole number of seconds"),
            ("500ms", "whole number of seconds"),
            ("0s", "longer than zero"),
            ("-5m", "longer than zero"),
        ] {
            let err = secs(input).unwrap_err();
            assert!(err.contains(reason), "{input:?} gave {err:?}");
        }
    }

    #[test]
    fn displays_largest_units_first_and_round_trips() {
        for (secs, text) in [
            (45, "45s"),
            (90, "1m 30s"),
            (900, "15m"),
            (5_400, "1h 30m"),
            (86_400, "1d"),
            (90_061, "1d 1h 1m 1s"),
        ] {
            let duration = Duration::from_secs(secs);
            assert_eq!(duration.to_string(), text);
            assert_eq!(Duration::parse(text), Ok(duration));
        }
    }

    #[test]
    fn the_error_names_the_input() {
        let err = "1mo".parse::<Duration>().unwrap_err();
        assert_eq!(err.code(), "invalid_duration");
        assert!(err.to_string().starts_with("invalid duration \"1mo\": "));
    }
}
