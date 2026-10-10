//! Schedules in plain English: "at 09:00 on the 1st of every month", "every 15 minutes".
//!
//! A description must never be wrong. Common shapes get a natural phrase; anything else falls back
//! to naming the values, which is clumsier but exact.

use super::field::{Field, MONTH_NAMES, Values, WEEKDAY_NAMES};
use super::{Calendar, Schedule};
use crate::duration::Duration;

/// Up to this many clock times, a schedule is described by listing them.
const MAX_LISTED_TIMES: usize = 6;

impl Schedule {
    /// The schedule in plain English, such as "at 09:00 on the 1st of every month".
    pub fn describe(&self) -> String {
        match self {
            Schedule::Calendar(calendar) => calendar.describe(),
            Schedule::Every(interval) => format!("every {}", in_words(*interval)),
        }
    }
}

/// When in the day a schedule runs.
enum Time {
    /// At listed clock times, "at 09:00 and 17:00", which needs the days saying even when it is
    /// every day.
    At(String),
    /// A repeating pattern, "every 15 minutes", which reads as every day on its own.
    Repeating(String),
}

impl Calendar {
    fn describe(&self) -> String {
        let months = (self.months != Field::Month.all()).then_some(self.months);
        let days = self.describe_days(months);
        let mut parts = Vec::new();
        match self.describe_time() {
            Time::At(time) => {
                parts.push(time);
                if days.is_none() {
                    parts.push("every day".to_owned());
                }
            }
            Time::Repeating(time) => parts.push(time),
        }
        match (days, months) {
            (Some(days), _) => parts.push(days),
            (None, Some(months)) => parts.push(month_phrase(months)),
            (None, None) => {}
        }
        parts.join(" ")
    }

    fn describe_time(&self) -> Time {
        let every_minute = self.minutes == Field::Minute.all();
        let every_hour = self.hours == Field::Hour.all();
        if every_minute {
            return Time::Repeating(if every_hour {
                "every minute".to_owned()
            } else {
                format!("every minute {}", hour_ranges(self.hours))
            });
        }
        let minute_step = step_from_zero(self.minutes, 60);
        if every_hour {
            return Time::Repeating(match minute_step {
                Some(step) => format!("every {step} minutes"),
                None if is_only(self.minutes, 0) => "every hour, on the hour".to_owned(),
                None => format!("every hour at {}", minute_list(self.minutes)),
            });
        }
        if self.minutes.len() * self.hours.len() <= MAX_LISTED_TIMES {
            let times = self
                .hours
                .iter()
                .flat_map(|hour| {
                    self.minutes
                        .iter()
                        .map(move |minute| format!("{hour:02}:{minute:02}"))
                })
                .collect();
            return Time::At(format!("at {}", list(times)));
        }
        if let Some(step) = minute_step {
            return Time::Repeating(format!("every {step} minutes {}", hour_ranges(self.hours)));
        }
        if let (Some(hour_step), 1) = (step_from_zero(self.hours, 24), self.minutes.len()) {
            return Time::Repeating(match self.minutes.iter().next() {
                Some(0) | None => format!("every {hour_step} hours, on the hour"),
                Some(minute) => format!("every {hour_step} hours at :{minute:02}"),
            });
        }
        Time::Repeating(format!(
            "at {} past hours {}",
            minute_list(self.minutes),
            list(
                self.hours
                    .runs()
                    .into_iter()
                    .map(|(first, last)| run(first, last, |hour| hour.to_string()))
                    .collect()
            )
        ))
    }

    /// Which days the schedule runs on, with its months folded in, or `None` for every day.
    fn describe_days(&self, months: Option<Values>) -> Option<String> {
        let every_day = self.days == Field::Day.all();
        let every_weekday = self.weekdays == Field::Weekday.all();
        let (by_day, by_weekday) = match (self.days_restricted, self.weekdays_restricted) {
            (false, false) => (false, false),
            (true, false) => (!every_day, false),
            (false, true) => (false, !every_weekday),
            (true, true) if every_day || every_weekday => (false, false),
            (true, true) => (true, true),
        };
        let phrase = match (by_day, by_weekday, months) {
            (false, false, _) => return None,
            (true, false, None) => format!("on {} of every month", ordinals(self.days)),
            (true, false, Some(months)) if months.len() == 1 => {
                format!("on {} of {}", ordinals(self.days), month_list(months))
            }
            (true, false, Some(months)) => format!(
                "on {} of every month {}",
                ordinals(self.days),
                month_phrase(months)
            ),
            (false, true, None) => format!("on {}", weekday_phrase(self.weekdays)),
            (false, true, Some(months)) => format!(
                "on {} {}",
                weekday_phrase(self.weekdays),
                month_phrase(months)
            ),
            (true, true, None) => format!(
                "on {} of every month, and on {}",
                ordinals(self.days),
                weekday_phrase(self.weekdays)
            ),
            (true, true, Some(months)) => format!(
                "{}, on {} and on {}",
                month_phrase(months),
                ordinals(self.days),
                weekday_phrase(self.weekdays)
            ),
        };
        Some(phrase)
    }
}

/// `step` when `values` is exactly every multiple of `step` from 0, and `step` divides `size`, so the
/// pattern repeats evenly: `*/15` in minutes. Not for single values or for every value.
fn step_from_zero(values: Values, size: u8) -> Option<u8> {
    let mut iter = values.iter();
    let (Some(0), Some(step)) = (iter.next(), iter.next()) else {
        return None;
    };
    if step < 2 || !size.is_multiple_of(step) {
        return None;
    }
    values
        .iter()
        .eq((0..size).step_by(usize::from(step)))
        .then_some(step)
}

fn is_only(values: Values, value: u8) -> bool {
    values.iter().eq([value])
}

/// "from 09:00 to 17:59", or several such ranges.
fn hour_ranges(hours: Values) -> String {
    list(
        hours
            .runs()
            .into_iter()
            .map(|(first, last)| format!("from {first:02}:00 to {last:02}:59"))
            .collect(),
    )
}

/// ":05 and :35", ":00 to :29".
fn minute_list(minutes: Values) -> String {
    list(
        minutes
            .runs()
            .into_iter()
            .map(|(first, last)| run(first, last, |minute| format!(":{minute:02}")))
            .collect(),
    )
}

/// "the 1st", "the 1st and 15th", "the 1st to 7th".
fn ordinals(days: Values) -> String {
    let days = list(
        days.runs()
            .into_iter()
            .map(|(first, last)| run(first, last, ordinal))
            .collect(),
    );
    format!("the {days}")
}

fn ordinal(day: u8) -> String {
    let suffix = match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{day}{suffix}")
}

/// "weekdays", "weekends", "Monday", "Monday to Wednesday and Friday".
fn weekday_phrase(weekdays: Values) -> String {
    if weekdays.iter().eq(1..=5) {
        return "weekdays".to_owned();
    }
    if weekdays.iter().eq([0, 6]) {
        return "weekends".to_owned();
    }
    list(
        weekdays
            .runs()
            .into_iter()
            .map(|(first, last)| {
                run(first, last, |weekday| {
                    WEEKDAY_NAMES[usize::from(weekday)].to_owned()
                })
            })
            .collect(),
    )
}

/// "January", "January and July", "March to May".
fn month_list(months: Values) -> String {
    list(
        months
            .runs()
            .into_iter()
            .map(|(first, last)| {
                run(first, last, |month| {
                    MONTH_NAMES[usize::from(month - 1)].to_owned()
                })
            })
            .collect(),
    )
}

/// "in January and July", or "from March to May" when the months are one run.
fn month_phrase(months: Values) -> String {
    match months.runs()[..] {
        [(first, last)] if last - first >= 2 => format!(
            "from {} to {}",
            MONTH_NAMES[usize::from(first - 1)],
            MONTH_NAMES[usize::from(last - 1)]
        ),
        _ => format!("in {}", month_list(months)),
    }
}

/// One run of consecutive values: "a" alone, "a and b" for two, "a to c" for three or more.
fn run(first: u8, last: u8, name: impl Fn(u8) -> String) -> String {
    match last - first {
        0 => name(first),
        1 => format!("{} and {}", name(first), name(last)),
        _ => format!("{} to {}", name(first), name(last)),
    }
}

/// "a", "a and b", "a, b and c".
fn list(items: Vec<String>) -> String {
    match items.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// "15 minutes", "hour", "1 hour and 30 minutes": what follows "every".
fn in_words(interval: Duration) -> String {
    let mut rest = interval.as_secs();
    let mut parts = Vec::new();
    for (unit, size) in [
        ("week", 604_800),
        ("day", 86_400),
        ("hour", 3_600),
        ("minute", 60),
        ("second", 1),
    ] {
        let count = rest / size;
        rest %= size;
        if count > 0 {
            parts.push((count, unit));
        }
    }
    if let [(1, unit)] = parts[..] {
        return unit.to_owned();
    }
    list(
        parts
            .into_iter()
            .map(|(count, unit)| match count {
                1 => format!("1 {unit}"),
                _ => format!("{count} {unit}s"),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn describe(text: &str) -> String {
        text.parse::<Schedule>().unwrap().describe()
    }

    #[test]
    fn describes_common_schedules_naturally() {
        for (text, expected) in [
            ("* * * * *", "every minute"),
            ("*/15 * * * *", "every 15 minutes"),
            ("0 * * * *", "every hour, on the hour"),
            ("5,35 * * * *", "every hour at :05 and :35"),
            ("0 */2 * * *", "every 2 hours, on the hour"),
            ("30 */6 * * *", "at 00:30, 06:30, 12:30 and 18:30 every day"),
            ("15 */3 * * *", "every 3 hours at :15"),
            ("0 9 * * *", "at 09:00 every day"),
            ("0 9,17 * * *", "at 09:00 and 17:00 every day"),
            ("@daily", "at 00:00 every day"),
            ("0 9 1 * *", "at 09:00 on the 1st of every month"),
            ("@monthly", "at 00:00 on the 1st of every month"),
            (
                "0 9 1,15 * *",
                "at 09:00 on the 1st and 15th of every month",
            ),
            ("0 0 1 1 *", "at 00:00 on the 1st of January"),
            ("@weekly", "at 00:00 on Sunday"),
            ("30 2 * * 1-5", "at 02:30 on weekdays"),
            ("0 10 * * SAT,SUN", "at 10:00 on weekends"),
            (
                "0 9 * * MON,WED,FRI",
                "at 09:00 on Monday, Wednesday and Friday",
            ),
            (
                "*/10 9-17 * * 1-5",
                "every 10 minutes from 09:00 to 17:59 on weekdays",
            ),
            ("* 9-17 * * *", "every minute from 09:00 to 17:59"),
            (
                "0 12 13 * 5",
                "at 12:00 on the 13th of every month, and on Friday",
            ),
            ("0 8 * 6-8 *", "at 08:00 every day from June to August"),
            ("0 8 * 1,7 *", "at 08:00 every day in January and July"),
            ("@every 15m", "every 15 minutes"),
            ("@every 1h", "every hour"),
            ("@every 90m", "every 1 hour and 30 minutes"),
            ("@every 2d", "every 2 days"),
        ] {
            assert_eq!(describe(text), expected, "{text:?}");
        }
    }

    #[test]
    fn falls_back_to_exact_values() {
        for (text, expected) in [
            (
                "0,20,45 9-17 * * *",
                "at :00, :20 and :45 past hours 9 to 17",
            ),
            (
                "0 1-23/2 * * *",
                "at :00 past hours 1, 3, 5, 7, 9, 11, 13, 15, 17, 19, 21 and 23",
            ),
            ("0-29 * * * *", "every hour at :00 to :29"),
            (
                "0 9 1-7 1,4,7,10 MON",
                "at 09:00 in January, April, July and October, on the 1st to 7th and on Monday",
            ),
        ] {
            assert_eq!(describe(text), expected, "{text:?}");
        }
    }

    #[test]
    fn a_day_field_that_allows_every_day_says_nothing_about_days() {
        assert_eq!(describe("0 9 1-31 * 1"), "at 09:00 every day");
        assert_eq!(
            describe("0 9 13 * *"),
            "at 09:00 on the 13th of every month"
        );
    }

    #[test]
    fn writes_ordinals() {
        let ordinals: Vec<_> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 31]
            .map(ordinal)
            .to_vec();
        assert_eq!(
            ordinals,
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "23rd", "31st"
            ]
        );
    }
}
