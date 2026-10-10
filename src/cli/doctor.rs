//! `macron doctor`: prints what [`diagnose`](crate::doctor::diagnose) found.

use super::Context;
use super::output::{self, Paint};
use crate::doctor::{self, Severity};
use crate::error::Result;

pub fn doctor(ctx: &Context) -> Result<()> {
    let diagnosis = doctor::diagnose(&ctx.macron);
    if ctx.json {
        return output::json(&diagnosis);
    }
    let paint = Paint::stdout();
    for check in &diagnosis.checks {
        let mark = match check.severity {
            Severity::Ok => paint.green("✓"),
            Severity::Warning => paint.yellow("!"),
            Severity::Problem => paint.red("✗"),
        };
        println!("{mark} {}", check.message);
        if let Some(fix) = &check.fix {
            println!("  {}", paint.dim(fix));
        }
    }
    for note in &diagnosis.notes {
        println!("\nNote: {note}");
    }
    let count = |severity: Severity| {
        diagnosis
            .checks
            .iter()
            .filter(|check| check.severity == severity)
            .count()
    };
    let (problems, warnings) = (count(Severity::Problem), count(Severity::Warning));
    let plural =
        |count: usize, word: &str| format!("{count} {word}{}", if count == 1 { "" } else { "s" });
    println!();
    match (problems, warnings) {
        (0, 0) => println!("Everything looks fine."),
        (0, warnings) => println!("{}, and no problems.", plural(warnings, "warning")),
        (problems, 0) => println!("{}.", plural(problems, "problem")),
        (problems, warnings) => println!(
            "{} and {}.",
            plural(problems, "problem"),
            plural(warnings, "warning")
        ),
    }
    Ok(())
}
