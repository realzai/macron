//! Checks macron's schedule semantics against croner, an independent cron implementation: for
//! random expressions and random start times, both must predict the same next runs.
//!
//! Only expressions both sides should accept are generated: standard syntax, increasing ranges, and
//! no `N/step` in the day-of-week field, where cron implementations disagree on whether the range
//! ends at 6 or at 7.

use std::str::FromStr;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use proptest::prelude::*;

use super::Calendar;

/// How many runs ahead each case compares.
const RUNS: usize = 5;

fn field(low: u8, high: u8) -> impl Strategy<Value = String> {
    let span = high - low + 1;
    prop_oneof![
        Just("*".to_owned()),
        (low..=high).prop_map(|value| value.to_string()),
        (low..=high, low..=high).prop_map(|(a, b)| format!("{}-{}", a.min(b), a.max(b))),
        (1..=span).prop_map(|step| format!("*/{step}")),
        (low..=high, low..=high, 1..=span).prop_map(|(a, b, step)| format!(
            "{}-{}/{step}",
            a.min(b),
            a.max(b)
        )),
        prop::collection::btree_set(low..=high, 2..5).prop_map(|values| {
            values
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",")
        }),
    ]
}

fn expression() -> impl Strategy<Value = String> {
    (
        field(0, 59),
        field(0, 23),
        field(1, 31),
        field(1, 12),
        field(0, 6),
    )
        .prop_map(|(minute, hour, day, month, weekday)| {
            format!("{minute} {hour} {day} {month} {weekday}")
        })
}

/// Start times from 2000 to 2100.
fn start() -> impl Strategy<Value = i64> {
    946_684_800_i64..4_102_444_800
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn next_runs_agree_with_croner(expression in expression(), start in start()) {
        let fields: [&str; 5] = expression
            .split_whitespace()
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let ours = Calendar::from_fields(fields).expect("macron should accept it");
        let theirs = croner::Cron::from_str(&expression).expect("croner should accept it");

        let start = Timestamp::from_second(start).unwrap().to_zoned(TimeZone::UTC);
        let (mut our_time, mut their_time) = (start.clone(), start);
        for _ in 0..RUNS {
            let ours = ours.next_after(&our_time);
            let theirs = theirs.find_next_occurrence(&their_time, false).ok();
            prop_assert_eq!(
                ours.as_ref().map(|time| time.timestamp()),
                theirs.as_ref().map(|time| time.timestamp()),
                "{} after {}", expression, our_time
            );
            match (ours, theirs) {
                (Some(ours), Some(theirs)) => (our_time, their_time) = (ours, theirs),
                _ => break,
            }
        }
    }
}
