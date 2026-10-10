//! The reference docs and the skill against the binary: every error code macron can print, and
//! every field of the JSON its commands print, must be in `docs/json.md`, and the skill must name
//! only codes that exist. (`docs/cli.md` is checked where it is generated, in
//! `src/cli/reference.rs`.)

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::Value;
use support::Sandbox;

fn read(file: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file)).unwrap()
}

/// The codes `Error::code` returns, read from its source, and the one for a command line that did
/// not parse.
fn error_codes() -> BTreeSet<String> {
    let source = read("src/error.rs");
    let start = source.find("pub fn code(").unwrap();
    let end = start + source[start..].find("\n    }\n").unwrap();
    let mut codes: BTreeSet<String> = source[start..end]
        .split("=> \"")
        .skip(1)
        .map(|rest| rest[..rest.find('"').unwrap()].to_owned())
        .collect();
    codes.insert("usage".to_owned());
    codes
}

/// The codes in the table under `## Errors`.
fn documented_codes(docs: &str) -> BTreeSet<String> {
    let start = docs.find("\n## Errors\n").unwrap();
    let section = &docs[start + 1..];
    let section = &section[..section[1..].find("\n## ").unwrap()];
    section
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .map(|rest| rest[..rest.find('`').unwrap()].to_owned())
        .collect()
}

#[test]
fn every_error_code_is_documented_and_every_documented_code_exists() {
    let codes = error_codes();
    assert!(
        codes.len() > 20,
        "found too few codes in src/error.rs: {codes:?}"
    );
    let documented = documented_codes(&read("docs/json.md"));
    let undocumented: Vec<_> = codes.difference(&documented).collect();
    let unknown: Vec<_> = documented.difference(&codes).collect();
    assert!(
        undocumented.is_empty(),
        "add these codes to the table in docs/json.md: {undocumented:?}"
    );
    assert!(
        unknown.is_empty(),
        "docs/json.md documents codes macron does not have: {unknown:?}"
    );
}

#[test]
fn the_skill_names_only_real_error_codes() {
    let skill = read("skills/macron/SKILL.md");
    let start = skill.find("\n## Errors you will meet\n").unwrap();
    let section = &skill[start + 1..];
    let section = &section[..section[1..].find("\n## ").unwrap()];
    let codes = error_codes();
    let named: Vec<String> = section
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        // The header row names its column `code`.
        .skip(1)
        .flat_map(|row| {
            let cell = &row[..row.find(" |").unwrap()];
            cell.split(", ")
                .map(|code| code.trim_matches('`').to_owned())
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        named.len() > 10,
        "found too few codes in the skill: {named:?}"
    );
    let unknown: Vec<_> = named.iter().filter(|code| !codes.contains(*code)).collect();
    assert!(
        unknown.is_empty(),
        "skills/macron/SKILL.md names codes macron does not have: {unknown:?}"
    );
}

/// Every object key in `value`, except the variable names in a job's `env`.
fn keys(value: &Value, into: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                into.insert(key.clone());
                if key != "env" {
                    keys(value, into);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                keys(item, into);
            }
        }
        _ => {}
    }
}

#[test]
fn every_field_printed_is_documented() {
    let sandbox = Sandbox::new();
    let commands: &[&[&str]] = &[
        &[
            "add",
            "backup",
            "--json",
            "-s",
            "@daily",
            "-d",
            "Back up",
            "-e",
            "MODE=full",
            "--timeout",
            "1m",
            "--shell",
            "/bin/sh",
            "--",
            "echo copying; echo oops >&2; exit 3",
        ],
        &["run", "backup", "--wait", "--json"],
        &["ls", "--json"],
        &["show", "backup", "--json"],
        &["edit", "backup", "-s", "@hourly", "--json"],
        &["disable", "backup", "--json"],
        &["enable", "backup", "--json"],
        &["runs", "--json"],
        &["logs", "backup", "--json"],
        &["apply", "--dry-run", "--json"],
        &["doctor", "--json"],
        &["paths", "--json"],
        &["rm", "backup", "--yes", "--purge", "--json"],
    ];

    let mut printed = BTreeSet::new();
    for args in commands {
        keys(&sandbox.run(args).json(), &mut printed);
    }
    let docs = read("docs/json.md");
    let undocumented: Vec<_> = printed
        .iter()
        .filter(|key| !docs.contains(&format!("`{key}`")) && !docs.contains(&format!(".{key}`")))
        .collect();
    assert!(
        undocumented.is_empty(),
        "document these fields in docs/json.md: {undocumented:?}"
    );
}
