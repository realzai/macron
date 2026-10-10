//! Schedules: parsing them, turning them into launchd's terms, describing them in English and
//! predicting when they run next.
//!
//! A schedule is one of:
//!
//! - five-field cron, `minute hour day-of-month month day-of-week`, with numbers, names (`JAN`,
//!   `MON`), `*`, ranges, lists and steps;
//! - a shorthand: `@hourly`, `@daily` (or `@midnight`), `@weekly`, `@monthly`, `@yearly` (or
//!   `@annually`);
//! - `@every <duration>`, such as `@every 15m`.
//!
//! launchd has no cron syntax. Its `StartCalendarInterval` takes one plain number per key, with no
//! ranges, lists or steps, so macron expands each restricted field into its values and emits one
//! entry per combination. A field that allows every value is left out, which launchd reads as "any".
//!
//! # Day of month and day of week
//!
//! As in cron, when both day fields are restricted a day matches if *either* does, and when only one
//! is restricted it alone decides. A field counts as restricted unless it is exactly `*`. Vixie
//! cron also treats a field starting with `*`, such as `*/2`, as unrestricted; macron does not,
//! because `*/2` plainly restricts the days.
//!
//! macron never relies on how launchd combines `Day` and `Weekday` within one entry. An entry
//! carries at most one of them. When both fields are restricted, the entries for each are emitted
//! side by side, so their union is cron's "either".

mod describe;
mod field;
#[cfg(test)]
mod oracle;

use std::str::FromStr;

use jiff::Zoned;
use jiff::civil::Date;
use serde::Serialize;

use crate::duration::Duration;
use crate::error::Error;
use field::{Field, Values};

/// The most launchd entries a calendar schedule may expand to. Far beyond what schedules people
/// write need (`*/5 9-17 * * 1-5` needs 540), it stops a mistyped field from producing a plist with
/// hundreds of thousands of entries.
pub const MAX_CALENDAR_INTERVALS: usize = 1_000;

/// The shortest `@every`. launchd does not start a job more often than every 10 seconds anyway.
pub const MIN_INTERVAL: Duration = Duration::from_secs(10);

/// How many days ahead [`Calendar::next_after`] looks. Eight years covers the longest gap a valid
/// schedule can have: 29 February across a century that is not a leap year, such as 2100.
const SEARCH_DAYS: usize = 8 * 366;

/// When a job runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    /// At wall-clock times: cron and the `@daily`-style shorthands.
    Calendar(Calendar),
    /// Every so often, counted by launchd from when it loaded the job: `@every`.
    Every(Duration),
}

/// A calendar schedule: the values each cron field allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    minutes: Values,
    hours: Values,
    days: Values,
    months: Values,
    weekdays: Values,
    days_restricted: bool,
    weekdays_restricted: bool,
}

/// One entry of launchd's `StartCalendarInterval`. A key left as `None` matches any value.
/// `weekday` counts from 0 for Sunday.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CalendarInterval {
    pub month: Option<u8>,
    pub day: Option<u8>,
    pub weekday: Option<u8>,
    pub hour: Option<u8>,
    pub minute: Option<u8>,
}

/// How launchd should start a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchdTrigger {
    /// `StartCalendarInterval`: run whenever any one entry matches.
    Calendar(Vec<CalendarInterval>),
    /// `StartInterval`, in seconds.
    Interval(u64),
}

impl FromStr for Schedule {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Error> {
        parse(input.trim()).map_err(|reason| Error::InvalidSchedule {
            schedule: input.to_owned(),
            reason,
        })
    }
}

impl Schedule {
    /// What launchd needs to start the job on this schedule.
    pub fn launchd_trigger(&self) -> LaunchdTrigger {
        match self {
            Schedule::Calendar(calendar) => LaunchdTrigger::Calendar(calendar.intervals()),
            Schedule::Every(interval) => LaunchdTrigger::Interval(interval.as_secs()),
        }
    }

    /// The first time after `after` that the schedule runs, in `after`'s time zone.
    ///
    /// `None` for `@every`, because launchd counts the interval from when it loaded the job, which
    /// the schedule alone does not know.
    pub fn next_after(&self, after: &Zoned) -> Option<Zoned> {
        match self {
            Schedule::Calendar(calendar) => calendar.next_after(after),
            Schedule::Every(_) => None,
        }
    }
}

fn parse(text: &str) -> Result<Schedule, String> {
    if text.is_empty() {
        return Err("it is empty".to_owned());
    }
    if let Some(shorthand) = text.strip_prefix('@') {
        return parse_shorthand(shorthand);
    }
    let fields: Vec<&str> = text.split_whitespace().collect();
    let fields: [&str; 5] = fields
        .try_into()
        .map_err(|fields: Vec<&str>| field_count_error(fields.len()))?;
    let calendar = Calendar::from_fields(fields)?;
    calendar.check()?;
    Ok(Schedule::Calendar(calendar))
}

fn field_count_error(count: usize) -> String {
    let expected = "five: minute, hour, day of month, month and day of week";
    match count {
        1 => format!("it has 1 field; macron takes {expected}"),
        6 => format!("it has 6 fields; macron takes {expected}, without seconds"),
        _ => format!("it has {count} fields; macron takes {expected}"),
    }
}

fn parse_shorthand(text: &str) -> Result<Schedule, String> {
    let (name, argument) = match text.split_once(char::is_whitespace) {
        Some((name, argument)) => (name, argument.trim()),
        None => (text, ""),
    };
    let cron = match name.to_ascii_lowercase().as_str() {
        "every" => return parse_every(argument),
        "hourly" => "0 * * * *",
        "daily" | "midnight" => "0 0 * * *",
        "weekly" => "0 0 * * 0",
        "monthly" => "0 0 1 * *",
        "yearly" | "annually" => "0 0 1 1 *",
        "reboot" => {
            return Err("`@reboot` is not supported; macron runs jobs on a schedule".to_owned());
        }
        _ => {
            return Err(format!(
                "`@{name}` is not a shorthand macron knows; use @hourly, @daily, @weekly, \
                 @monthly, @yearly or @every <duration>"
            ));
        }
    };
    if !argument.is_empty() {
        return Err(format!("`@{name}` takes nothing after it"));
    }
    parse(cron)
}

fn parse_every(argument: &str) -> Result<Schedule, String> {
    if argument.is_empty() {
        return Err("`@every` needs a duration, such as `@every 15m`".to_owned());
    }
    let interval = Duration::parse(argument)?;
    if interval < MIN_INTERVAL {
        return Err(format!(
            "it must be at least {MIN_INTERVAL}; launchd does not start a job more often than that"
        ));
    }
    Ok(Schedule::Every(interval))
}

impl Calendar {
    /// Parses the five fields, without checking that the result ever runs or fits in launchd.
    fn from_fields(fields: [&str; 5]) -> Result<Self, String> {
        let parse = |field: Field, text: &str| {
            field::parse(field, text).map_err(|reason| format!("{}: {reason}", field.label()))
        };
        Ok(Self {
            minutes: parse(Field::Minute, fields[0])?,
            hours: parse(Field::Hour, fields[1])?,
            days: parse(Field::Day, fields[2])?,
            months: parse(Field::Month, fields[3])?,
            weekdays: parse(Field::Weekday, fields[4])?,
            days_restricted: fields[2] != "*",
            weekdays_restricted: fields[4] != "*",
        })
    }

    /// Refuses a schedule that never runs, or that launchd would need too many entries for.
    fn check(&self) -> Result<(), String> {
        if !self.can_run() {
            return Err(
                "it never runs: none of its months has any of its days of month".to_owned(),
            );
        }
        let entries = self.interval_count();
        if entries > MAX_CALENDAR_INTERVALS {
            return Err(format!(
                "it needs {entries} launchd entries, more than the {MAX_CALENDAR_INTERVALS} \
                 macron allows; simplify it, or use `@every`"
            ));
        }
        Ok(())
    }

    fn can_run(&self) -> bool {
        if !self.days_restricted || self.weekdays_restricted {
            return true;
        }
        self.months
            .iter()
            .any(|month| self.days.iter().any(|day| day <= longest_month(month)))
    }

    fn intervals(&self) -> Vec<CalendarInterval> {
        let months = keys(self.months, Field::Month);
        let days = self.day_keys();
        let hours = keys(self.hours, Field::Hour);
        let minutes = keys(self.minutes, Field::Minute);
        let mut intervals =
            Vec::with_capacity(months.len() * days.len() * hours.len() * minutes.len());
        for &month in &months {
            for &(day, weekday) in &days {
                for &hour in &hours {
                    for &minute in &minutes {
                        intervals.push(CalendarInterval {
                            month,
                            day,
                            weekday,
                            hour,
                            minute,
                        });
                    }
                }
            }
        }
        intervals
    }

    fn interval_count(&self) -> usize {
        [
            (self.months, Field::Month),
            (self.hours, Field::Hour),
            (self.minutes, Field::Minute),
        ]
        .into_iter()
        .map(|(values, field)| keys(values, field).len())
        .product::<usize>()
            * self.day_keys().len()
    }

    /// The `(day, weekday)` keys the entries need, following the day-matching rule in the module
    /// documentation. Any `(None, None)` pair means every day, which makes the others redundant.
    fn day_keys(&self) -> Vec<(Option<u8>, Option<u8>)> {
        let by_day = || {
            keys(self.days, Field::Day)
                .into_iter()
                .map(|day| (day, None))
        };
        let by_weekday = || {
            keys(self.weekdays, Field::Weekday)
                .into_iter()
                .map(|weekday| (None, weekday))
        };
        let pairs: Vec<_> = match (self.days_restricted, self.weekdays_restricted) {
            (false, false) => vec![(None, None)],
            (true, false) => by_day().collect(),
            (false, true) => by_weekday().collect(),
            (true, true) => by_day().chain(by_weekday()).collect(),
        };
        if pairs.contains(&(None, None)) {
            vec![(None, None)]
        } else {
            pairs
        }
    }

    fn runs_on(&self, date: Date) -> bool {
        if !self.months.contains(date.month() as u8) {
            return false;
        }
        let on_day = self.days.contains(date.day() as u8);
        let on_weekday = self
            .weekdays
            .contains(date.weekday().to_sunday_zero_offset() as u8);
        match (self.days_restricted, self.weekdays_restricted) {
            (true, true) => on_day || on_weekday,
            (true, false) => on_day,
            (false, true) => on_weekday,
            (false, false) => true,
        }
    }

    /// The first time after `after` that the schedule runs, in `after`'s time zone.
    ///
    /// A wall-clock time skipped by a daylight-saving change runs at the same offset after the
    /// change (02:30 becomes 03:30), and a repeated one runs the first time it occurs. launchd
    /// decides for itself on those nights, so the prediction can be off then.
    pub fn next_after(&self, after: &Zoned) -> Option<Zoned> {
        let time_zone = after.time_zone();
        let mut date = after.date();
        for _ in 0..SEARCH_DAYS {
            if self.runs_on(date) {
                for hour in self.hours.iter() {
                    for minute in self.minutes.iter() {
                        let time = date
                            .at(hour as i8, minute as i8, 0, 0)
                            .to_zoned(time_zone.clone())
                            .ok()?;
                        if time.timestamp() > after.timestamp() {
                            return Some(time);
                        }
                    }
                }
            }
            date = date.tomorrow().ok()?;
        }
        None
    }
}

/// The values to emit for one key: one per value, or a single wildcard when every value is allowed.
fn keys(values: Values, field: Field) -> Vec<Option<u8>> {
    if values == field.all() {
        vec![None]
    } else {
        values.iter().map(Some).collect()
    }
}

/// The most days a month can have, counting 29 February.
fn longest_month(month: u8) -> u8 {
    match month {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;

    use super::*;

    fn schedule(text: &str) -> Schedule {
        text.parse()
            .unwrap_or_else(|err| panic!("{text:?} should parse: {err}"))
    }

    fn reason(text: &str) -> String {
        match text.parse::<Schedule>() {
            Ok(_) => panic!("{text:?} should not parse"),
            Err(Error::InvalidSchedule { reason, .. }) => reason,
            Err(other) => panic!("{text:?} gave {other:?}"),
        }
    }

    fn intervals(text: &str) -> Vec<CalendarInterval> {
        match schedule(text).launchd_trigger() {
            LaunchdTrigger::Calendar(intervals) => intervals,
            LaunchdTrigger::Interval(_) => panic!("{text:?} is not a calendar schedule"),
        }
    }

    fn at(hour: u8, minute: u8) -> CalendarInterval {
        CalendarInterval {
            hour: Some(hour),
            minute: Some(minute),
            ..CalendarInterval::default()
        }
    }

    fn zoned(text: &str) -> Zoned {
        text.parse().unwrap()
    }

    fn next(text: &str, after: &str) -> String {
        schedule(text)
            .next_after(&zoned(after))
            .expect("should run")
            .to_string()
    }

    #[test]
    fn a_fixed_time_is_one_entry() {
        assert_eq!(
            intervals("0 9 1 * *"),
            [CalendarInterval {
                day: Some(1),
                ..at(9, 0)
            }]
        );
    }

    #[test]
    fn fields_that_allow_everything_are_left_out() {
        assert_eq!(intervals("* * * * *"), [CalendarInterval::default()]);
        assert_eq!(
            intervals("0-59 */1 1-31 * 0-7"),
            [CalendarInterval::default()]
        );
    }

    #[test]
    fn steps_expand_to_one_entry_per_value() {
        let minutes: Vec<_> = intervals("*/15 * * * *")
            .iter()
            .map(|entry| entry.minute)
            .collect();
        assert_eq!(minutes, [Some(0), Some(15), Some(30), Some(45)]);
        assert_eq!(intervals("*/5 9-17 * * 1-5").len(), 12 * 9 * 5);
    }

    #[test]
    fn one_restricted_day_field_alone_decides() {
        let weekdays: Vec<_> = intervals("0 9 * * MON-FRI")
            .iter()
            .map(|entry| (entry.day, entry.weekday))
            .collect();
        assert_eq!(
            weekdays,
            [1, 2, 3, 4, 5].map(|weekday| (None, Some(weekday)))
        );
    }

    #[test]
    fn two_restricted_day_fields_become_separate_entries() {
        assert_eq!(
            intervals("0 9 13 * 5"),
            [
                CalendarInterval {
                    day: Some(13),
                    ..at(9, 0)
                },
                CalendarInterval {
                    weekday: Some(5),
                    ..at(9, 0)
                },
            ]
        );
    }

    #[test]
    fn a_restricted_day_field_that_allows_every_day_means_every_day() {
        assert_eq!(intervals("0 9 1-31 * 1"), [at(9, 0)]);
        assert_eq!(intervals("0 9 13 * 0-6"), [at(9, 0)]);
    }

    #[test]
    fn months_combine_with_days() {
        assert_eq!(
            intervals("0 0 1 1,7 *"),
            [1, 7].map(|month| CalendarInterval {
                month: Some(month),
                day: Some(1),
                ..at(0, 0)
            })
        );
    }

    #[test]
    fn shorthands_are_their_cron_equivalents() {
        for (shorthand, cron) in [
            ("@hourly", "0 * * * *"),
            ("@daily", "0 0 * * *"),
            ("@midnight", "0 0 * * *"),
            ("@weekly", "0 0 * * 0"),
            ("@monthly", "0 0 1 * *"),
            ("@yearly", "0 0 1 1 *"),
            ("@annually", "0 0 1 1 *"),
            ("@DAILY", "0 0 * * *"),
        ] {
            assert_eq!(schedule(shorthand), schedule(cron), "{shorthand}");
        }
    }

    #[test]
    fn every_is_a_launchd_interval() {
        assert_eq!(
            schedule("@every 15m").launchd_trigger(),
            LaunchdTrigger::Interval(900)
        );
        assert_eq!(
            schedule("@every 1h 30m").launchd_trigger(),
            LaunchdTrigger::Interval(5_400)
        );
        assert_eq!(schedule("@every 15m").next_after(&Zoned::now()), None);
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(schedule("  0 9 * * *\n"), schedule("0 9 * * *"));
    }

    #[test]
    fn explains_what_is_wrong() {
        for (text, expected) in [
            ("", "it is empty"),
            (
                "0 9 * *",
                "it has 4 fields; macron takes five: minute, hour, day of month, month and day of week",
            ),
            (
                "0 0 9 * * *",
                "it has 6 fields; macron takes five: minute, hour, day of month, month and day of week, without seconds",
            ),
            ("0 25 * * *", "hour: `25` is out of range; use 0–23"),
            (
                "0 0 30 2 *",
                "it never runs: none of its months has any of its days of month",
            ),
            (
                "@reboot",
                "`@reboot` is not supported; macron runs jobs on a schedule",
            ),
            (
                "@often",
                "`@often` is not a shorthand macron knows; use @hourly, @daily, @weekly, @monthly, @yearly or @every <duration>",
            ),
            ("@daily 9am", "`@daily` takes nothing after it"),
            ("@every", "`@every` needs a duration, such as `@every 15m`"),
            (
                "@every 1mo",
                "months and years have no fixed length; use days or weeks",
            ),
            (
                "@every 5s",
                "it must be at least 10s; launchd does not start a job more often than that",
            ),
        ] {
            assert_eq!(reason(text), expected, "{text:?}");
        }
    }

    #[test]
    fn refuses_a_schedule_needing_too_many_entries() {
        assert!(reason("*/2 */2 * * 1-5").contains("needs 1800 launchd entries"));
    }

    #[test]
    fn the_error_names_the_schedule() {
        let err = "0 25 * * *".parse::<Schedule>().unwrap_err();
        assert_eq!(err.code(), "invalid_schedule");
        assert_eq!(
            err.to_string(),
            "invalid schedule \"0 25 * * *\": hour: `25` is out of range; use 0–23"
        );
    }

    #[test]
    fn predicts_the_next_run() {
        let after = "2026-10-10T09:00:00+00:00[UTC]";
        assert_eq!(next("0 9 * * *", after), "2026-10-11T09:00:00+00:00[UTC]");
        assert_eq!(
            next("*/15 * * * *", after),
            "2026-10-10T09:15:00+00:00[UTC]"
        );
        assert_eq!(next("0 9 1 * *", after), "2026-11-01T09:00:00+00:00[UTC]");
        // 2026-10-10 is a Saturday.
        assert_eq!(
            next("30 8 * * MON", after),
            "2026-10-12T08:30:00+00:00[UTC]"
        );
        // Either day field matching is enough: Friday the 16th comes before the 13th of November.
        assert_eq!(
            next("0 0 13 * FRI", "2026-10-14T00:00:00+00:00[UTC]"),
            "2026-10-16T00:00:00+00:00[UTC]"
        );
    }

    #[test]
    fn predicts_runs_years_apart() {
        // 2100 is not a leap year, so 29 February skips from 2096 to 2104.
        assert_eq!(
            next("0 0 29 2 *", "2096-03-01T00:00:00+00:00[UTC]"),
            "2104-02-29T00:00:00+00:00[UTC]"
        );
    }

    #[test]
    fn predicts_in_the_time_zone_given() {
        let tz = TimeZone::get("America/New_York").unwrap();
        // 12:00 in UTC is 08:00 in New York, so 09:00 there is still to come.
        let after = zoned("2026-07-01T12:00:00+00:00[UTC]").with_time_zone(tz);
        let next = schedule("0 9 * * *").next_after(&after).unwrap();
        assert_eq!(
            next.to_string(),
            "2026-07-01T09:00:00-04:00[America/New_York]"
        );
    }

    #[test]
    fn a_time_skipped_by_daylight_saving_runs_after_the_change() {
        assert_eq!(
            next("30 2 * * *", "2026-03-08T00:00:00-05:00[America/New_York]"),
            "2026-03-08T03:30:00-04:00[America/New_York]"
        );
    }

    #[test]
    fn a_time_repeated_by_daylight_saving_runs_the_first_time() {
        assert_eq!(
            next("30 1 * * *", "2026-11-01T00:00:00-04:00[America/New_York]"),
            "2026-11-01T01:30:00-04:00[America/New_York]"
        );
    }
}
