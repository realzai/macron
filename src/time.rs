//! Times as macron writes them for machines: RFC 3339 in the Mac's time zone, to the second.

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// `time` as RFC 3339 with its offset: `2026-10-10T23:14:00+01:00`.
pub fn rfc3339(time: &Zoned) -> String {
    time.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// `time` in the Mac's time zone.
pub fn local(time: Timestamp) -> Zoned {
    time.to_zoned(TimeZone::system())
}

/// Serializes a timestamp as [`rfc3339`] in the Mac's time zone.
pub(crate) fn serialize<S: serde::Serializer>(
    time: &Timestamp,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&rfc3339(&local(*time)))
}

/// Serializes an optional timestamp as [`rfc3339`] in the Mac's time zone, or `null`.
pub(crate) fn serialize_option<S: serde::Serializer>(
    time: &Option<Timestamp>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match time {
        Some(time) => serialize(time, serializer),
        None => serializer.serialize_none(),
    }
}

/// Serializes an optional zoned time as [`rfc3339`] in its own time zone, or `null`.
pub(crate) fn serialize_zoned_option<S: serde::Serializer>(
    time: &Option<Zoned>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match time {
        Some(time) => serializer.serialize_str(&rfc3339(time)),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_rfc3339_with_the_offset() {
        let time: Zoned = "2026-10-10T23:14:05.678+01:00[Europe/London]"
            .parse()
            .unwrap();
        assert_eq!(rfc3339(&time), "2026-10-10T23:14:05+01:00");
    }
}
