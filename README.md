# macron

Schedule, run and watch local jobs on macOS.

macron is cron for a Mac that sleeps. You describe your jobs once; macron installs them into
launchd, which runs them on schedule, including runs missed while the Mac was asleep. Every command
speaks JSON, so scripts and agents can drive it as easily as you can.

> **Status: early development.** Jobs can be added, changed and installed into launchd today. Run
> history, `macron logs` and the `macron up` web interface are next. The plan, including what is
> being built and in what order, is in [`docs/plan.md`](docs/plan.md).

## Why not cron?

cron skips any job whose time comes while your Mac is asleep, and tells you nothing when a job
fails. launchd, which macOS ships, runs a missed job when the Mac wakes, but it is configured with
XML files and `launchctl`. macron gives you cron's schedules on top of launchd.

## Install

From source, with a [Rust toolchain](https://rustup.rs):

```sh
cargo install --git https://github.com/realzai/macron
```

## Use

```sh
# Add a job: a name, a schedule, and the command after --.
macron add backup --schedule '0 9 1 * *' -- ~/bin/backup.sh
macron add sweep -s '@every 15m' --timeout 5m -- 'cd ~/inbox && ./sweep --all'

macron ls                  # every job, its next run and its state
macron show backup         # one job in full
macron edit backup -s '0 10 1 * *'
macron disable sweep       # uninstall, but keep it in jobs.toml
macron enable sweep
macron rm backup
```

```
NAME    SCHEDULE    NEXT RUN         STATE   WHEN
backup  0 9 1 * *   Sun 1 Nov 09:00  active  at 09:00 on the 1st of every month
sweep   @every 15m  -                active  every 15 minutes
```

Schedules are five-field cron (`minute hour day-of-month month day-of-week`, with names such as
`MON` and `JAN`), a shorthand (`@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`), or
`@every <duration>`.

Jobs are kept in `~/.config/macron/jobs.toml`, which you can edit by hand. Run `macron apply`
afterwards to bring launchd in line with it; edits from the CLI keep your comments and formatting.

```toml
[jobs.backup]
schedule = "0 9 1 * *"
command = "~/bin/backup.sh"
description = "Monthly photo backup"
timeout = "2h"
env = { BACKUP_TARGET = "/Volumes/Backup" }
```

Each command runs in a login shell (`/bin/zsh -lc` by default), so your `PATH`, Homebrew's
included, is set up as in a terminal. Add `--json` to any command for machine-readable output.

## Good to know

- **Sleep and power-off.** launchd runs a job missed while the Mac slept, once, when it wakes. A job
  missed while the Mac was off is not run.
- **Schedule changes need launchd to hear about them; command changes do not.** `macron add`,
  `edit`, `enable` and `disable` update launchd themselves. After editing `jobs.toml` by hand, run
  `macron apply`. A job's command is read when it runs, so a changed command takes effect on the
  next run either way.

## Contributing

See [`AGENTS.md`](AGENTS.md). It applies to people as much as to agents.
