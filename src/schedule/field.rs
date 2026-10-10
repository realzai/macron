//! One field of a cron expression, and the set of values it allows.

/// The five fields of a cron expression, in the order they are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    Minute,
    Hour,
    Day,
    Month,
    Weekday,
}

pub(crate) const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

pub(crate) const WEEKDAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

impl Field {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Field::Minute => "minute",
            Field::Hour => "hour",
            Field::Day => "day of month",
            Field::Month => "month",
            Field::Weekday => "day of week",
        }
    }

    /// The lowest and highest value the field holds. Day of week runs from 0 (Sunday) to 6.
    pub(crate) fn bounds(self) -> (u8, u8) {
        match self {
            Field::Minute => (0, 59),
            Field::Hour => (0, 23),
            Field::Day => (1, 31),
            Field::Month => (1, 12),
            Field::Weekday => (0, 6),
        }
    }

    /// Every value the field holds.
    pub(crate) fn all(self) -> Values {
        let (low, high) = self.bounds();
        let mut values = Values::default();
        for value in low..=high {
            values.insert(value);
        }
        values
    }

    /// The highest number accepted when parsing. Day of week also takes 7 for Sunday, as cron does.
    fn written_max(self) -> u8 {
        match self {
            Field::Weekday => 7,
            _ => self.bounds().1,
        }
    }

    /// Names accepted in place of numbers, with the value of the first.
    fn names(self) -> Option<(&'static [&'static str], u8)> {
        match self {
            Field::Month => Some((&MONTH_NAMES, 1)),
            Field::Weekday => Some((&WEEKDAY_NAMES, 0)),
            _ => None,
        }
    }

    fn accepted(self) -> String {
        let (low, _) = self.bounds();
        let max = self.written_max();
        match self {
            Field::Month => format!("{low}–{max} or JAN–DEC"),
            Field::Weekday => format!("{low}–{max} or SUN–SAT"),
            _ => format!("{low}–{max}"),
        }
    }
}

/// The values a field allows, as a bitset: bit `n` is set when value `n` is allowed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(crate) struct Values(u64);

impl Values {
    pub(crate) fn contains(self, value: u8) -> bool {
        value < 64 && self.0 & (1 << value) != 0
    }

    pub(crate) fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// The values in ascending order.
    pub(crate) fn iter(self) -> impl Iterator<Item = u8> {
        (0..64).filter(move |&value| self.contains(value))
    }

    /// The values as ascending runs of consecutive values, each as `(first, last)`.
    pub(crate) fn runs(self) -> Vec<(u8, u8)> {
        let mut runs: Vec<(u8, u8)> = Vec::new();
        for value in self.iter() {
            match runs.last_mut() {
                Some((_, last)) if *last + 1 == value => *last = value,
                _ => runs.push((value, value)),
            }
        }
        runs
    }

    fn insert(&mut self, value: u8) {
        self.0 |= 1 << value;
    }

    fn remove(&mut self, value: u8) {
        self.0 &= !(1 << value);
    }
}

/// Parses one field: a comma-separated list of `*`, values, ranges (`1-5`) and steps (`*/15`,
/// `1-30/5`, and `5/15`, meaning from 5 to the field's end). Returns why it is invalid on failure.
pub(crate) fn parse(field: Field, text: &str) -> Result<Values, String> {
    let mut values = Values::default();
    for item in text.split(',') {
        parse_item(field, item, &mut values)?;
    }
    if field == Field::Weekday && values.contains(7) {
        values.remove(7);
        values.insert(0);
    }
    if values.len() == 0 {
        return Err(format!("`{text}` allows no values"));
    }
    Ok(values)
}

fn parse_item(field: Field, item: &str, values: &mut Values) -> Result<(), String> {
    if item.is_empty() {
        return Err("a list has an empty item".to_owned());
    }
    let (range, step) = match item.split_once('/') {
        Some((range, step)) => (range, Some(parse_step(step)?)),
        None => (item, None),
    };
    let (first, last) = if range == "*" {
        field.bounds()
    } else if let Some((first, last)) = range.split_once('-') {
        let (first, last) = (value(field, first)?, value(field, last)?);
        if first > last {
            return Err(format!(
                "`{range}` runs backwards; write the smaller value first"
            ));
        }
        (first, last)
    } else {
        let first = value(field, range)?;
        let last = if step.is_some() {
            field.bounds().1
        } else {
            first
        };
        (first, last)
    };
    for value in (first..=last).step_by(usize::from(step.unwrap_or(1))) {
        values.insert(value);
    }
    Ok(())
}

fn parse_step(text: &str) -> Result<u8, String> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("step `{text}` is not a number"));
    }
    match text.parse::<u8>() {
        Ok(0) => Err("a step must be at least 1".to_owned()),
        Ok(step) => Ok(step),
        Err(_) => Err(format!("step `{text}` is too large")),
    }
}

fn value(field: Field, text: &str) -> Result<u8, String> {
    if text.is_empty() {
        return Err(format!("a value is missing; use {}", field.accepted()));
    }
    let (low, _) = field.bounds();
    let max = field.written_max();
    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return match text.parse::<u8>() {
            Ok(value) if (low..=max).contains(&value) => Ok(value),
            _ => Err(format!(
                "`{text}` is out of range; use {}",
                field.accepted()
            )),
        };
    }
    if let Some((names, first)) = field.names()
        && let Some(index) = names
            .iter()
            .position(|name| name[..3].eq_ignore_ascii_case(text))
    {
        return Ok(first + index as u8);
    }
    if text.contains(['L', 'W', '#', '?']) {
        return Err(format!(
            "`{text}` is not supported; macron takes standard five-field cron, \
             without the `L`, `W`, `#` and `?` extensions"
        ));
    }
    Err(format!(
        "`{text}` is not a valid {}; use {}",
        field.label(),
        field.accepted()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(field: Field, text: &str) -> Vec<u8> {
        parse(field, text).unwrap().iter().collect()
    }

    fn error(field: Field, text: &str) -> String {
        parse(field, text).unwrap_err()
    }

    #[test]
    fn parses_values_ranges_lists_and_steps() {
        assert_eq!(parsed(Field::Minute, "5"), [5]);
        assert_eq!(parsed(Field::Minute, "*/15"), [0, 15, 30, 45]);
        assert_eq!(parsed(Field::Minute, "10-20/5"), [10, 15, 20]);
        assert_eq!(parsed(Field::Minute, "50/5"), [50, 55]);
        assert_eq!(parsed(Field::Hour, "9-11,14"), [9, 10, 11, 14]);
        assert_eq!(parsed(Field::Day, "*/10"), [1, 11, 21, 31]);
        assert_eq!(parsed(Field::Month, "*"), (1..=12).collect::<Vec<_>>());
        assert_eq!(parsed(Field::Minute, "*/60"), [0]);
    }

    #[test]
    fn accepts_month_and_weekday_names_in_any_case() {
        assert_eq!(parsed(Field::Month, "JAN,jul"), [1, 7]);
        assert_eq!(parsed(Field::Month, "Mar-May"), [3, 4, 5]);
        assert_eq!(parsed(Field::Weekday, "mon-FRI"), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn day_of_week_7_is_sunday() {
        assert_eq!(parsed(Field::Weekday, "7"), [0]);
        assert_eq!(parsed(Field::Weekday, "5-7"), [0, 5, 6]);
        assert_eq!(parsed(Field::Weekday, "0-7"), (0..=6).collect::<Vec<_>>());
        assert_eq!(parse(Field::Weekday, "0-7"), Ok(Field::Weekday.all()));
    }

    #[test]
    fn explains_what_is_wrong() {
        for (field, text, expected) in [
            (Field::Minute, "60", "`60` is out of range; use 0–59"),
            (Field::Day, "0", "`0` is out of range; use 1–31"),
            (
                Field::Weekday,
                "8",
                "`8` is out of range; use 0–7 or SUN–SAT",
            ),
            (
                Field::Month,
                "Foo",
                "`Foo` is not a valid month; use 1–12 or JAN–DEC",
            ),
            (Field::Hour, "+5", "`+5` is not a valid hour; use 0–23"),
            (
                Field::Hour,
                "17-9",
                "`17-9` runs backwards; write the smaller value first",
            ),
            (Field::Minute, "*/0", "a step must be at least 1"),
            (Field::Minute, "*/x", "step `x` is not a number"),
            (Field::Minute, "1,,2", "a list has an empty item"),
            (Field::Minute, "5-", "a value is missing; use 0–59"),
            (Field::Weekday, "7/2", "`7/2` allows no values"),
        ] {
            assert_eq!(error(field, text), expected, "{} {text:?}", field.label());
        }
    }

    #[test]
    fn points_cron_extensions_at_standard_cron() {
        for (field, text) in [
            (Field::Day, "L"),
            (Field::Day, "15W"),
            (Field::Weekday, "5#2"),
            (Field::Day, "?"),
        ] {
            assert!(error(field, text).contains("not supported"), "{text}");
        }
    }

    #[test]
    fn groups_values_into_runs() {
        let values = parse(Field::Minute, "1,2,3,7,9,10").unwrap();
        assert_eq!(values.runs(), [(1, 3), (7, 7), (9, 10)]);
    }
}
