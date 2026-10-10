//! `docs/cli.md`, generated from the command definitions so it cannot drift from the binary.
//!
//! The test below fails when the file is out of date. Regenerate it with:
//!
//! ```sh
//! MACRON_UPDATE_DOCS=1 cargo test --lib cli_reference
//! ```

use std::fs;
use std::path::Path;

use clap::CommandFactory;

use super::Cli;

/// The width the reference is wrapped at, fixed so it does not depend on the terminal it is
/// generated in.
const WIDTH: usize = 100;

fn markdown() -> String {
    let mut cli = Cli::command().term_width(WIDTH);
    cli.build();
    let mut out = String::from(
        "# Command-line reference\n\n\
         <!-- Generated from src/cli by `MACRON_UPDATE_DOCS=1 cargo test --lib cli_reference`. \
         Do not edit by hand. -->\n\n\
         Every command takes `--json`, which prints JSON instead of text. The JSON each command \
         prints, and every error code, are in [json.md](json.md).\n\n",
    );
    let commands: Vec<_> = cli
        .get_subcommands()
        .filter(|command| !command.is_hide_set() && command.get_name() != "help")
        .collect();
    for command in &commands {
        let about = command
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default();
        out.push_str(&format!(
            "- [`macron {name}`](#macron-{name}): {about}\n",
            name = command.get_name()
        ));
    }
    out.push_str(&format!("\n## `macron`\n\n{}", help(&mut cli.clone())));
    for command in commands {
        out.push_str(&format!(
            "\n## `macron {}`\n\n{}",
            command.get_name(),
            help(&mut command.clone())
        ));
    }
    out
}

/// A command's full help, as `--help` prints it, in a code block.
fn help(command: &mut clap::Command) -> String {
    let text = command.render_long_help().to_string();
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    format!("```text\n{}\n```\n", lines.join("\n").trim_end())
}

#[test]
fn cli_reference_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/cli.md");
    let generated = markdown();
    if std::env::var_os("MACRON_UPDATE_DOCS").is_some() {
        fs::write(&path, &generated).unwrap();
        return;
    }
    let current = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        current == generated,
        "docs/cli.md is out of date with the command definitions. Regenerate it with:\n\
         MACRON_UPDATE_DOCS=1 cargo test --lib cli_reference"
    );
}
