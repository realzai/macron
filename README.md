# macron

Schedule, run and watch local jobs on macOS.

macron is cron for a Mac that sleeps. You describe your jobs once; macron installs them into
launchd, which runs them on schedule, including runs missed while the Mac was asleep. Every run is
recorded with its output, so you can see what happened. Every command speaks JSON, so scripts and
agents can drive it as easily as you can.

> **Status: early development.** Jobs, run history and logs work from the command line today. The
> `macron up` web interface is next. The plan, including what is being built and in what order, is
> in [`docs/plan.md`](docs/plan.md).

## Why not cron?

cron skips any job whose time comes while your Mac is asleep, and tells you nothing when a job
fails. launchd, which macOS ships, runs a missed job when the Mac wakes, but it is configured with
XML files and `launchctl`, and keeps no history. macron gives you cron's schedules on top of
launchd, a record of every run, and a notification when one fails.

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
NAME    SCHEDULE    NEXT RUN         LAST RUN               STATE
backup  0 9 1 * *   Sun 1 Nov 09:00  succeeded 9d ago       active
sweep   @every 15m  -                failed, exit 1 2m ago  active
```

### Runs and logs

```sh
macron run backup            # run it now, through launchd, as its schedule would
macron run backup --wait     # ...and watch its output, exiting as it did
macron runs                  # recent runs of every job
macron runs --status failed
macron logs sweep            # the latest run's output, stderr in red
macron logs sweep --follow   # keep printing until the run ends
macron logs sweep --run 41 --raw | grep error
```

Each run's output is kept, up to 10 MiB, along with when it ran, how long it took and how it ended.
The newest 100 runs of each job are kept; set `keep_runs` to change that. By default macOS posts a
notification when a run fails; set `notify` to `always` or `never` to change that.

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
notify = "always"
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

## License

[MIT](LICENSE)
