//! Rendering: JSON for scripts and agents, aligned text for people.

use std::io::{self, IsTerminal};

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

    pub fn green(self, text: &str) -> String {
        self.paint("32", text)
    }

    pub fn yellow(self, text: &str) -> String {
        self.paint("33", text)
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}
