//! Rendering: JSON for scripts and agents, aligned text for people.

use std::path::Path;

use jiff::Zoned;
use serde::Serialize;

use crate::error::Result;

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

/// A time for JSON: RFC 3339, in local time with its offset.
pub fn json_time(time: &Zoned) -> String {
    time.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
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
        assert_eq!(json_time(&now), "2026-10-10T12:00:00+01:00");
    }

    #[test]
    fn abbreviates_home() {
        let home = Path::new("/Users/ada");
        assert_eq!(tilde(Path::new("/Users/ada"), home), "~");
        assert_eq!(tilde(Path::new("/Users/ada/x/y"), home), "~/x/y");
        assert_eq!(tilde(Path::new("/tmp/x"), home), "/tmp/x");
    }
}
