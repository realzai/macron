# macron

Schedule, run and watch local jobs on macOS.

macron is cron for a Mac that sleeps. You describe your jobs once; macron installs them into
launchd, records every run with its output, and lets you browse it all from the terminal or the
browser. Type `macron up` and manage everything from a local web page. Agents can drive it too:
every command speaks JSON.

> **Status: early development.** Nothing here is usable yet. The plan, including what is being
> built and in what order, is in [`docs/plan.md`](docs/plan.md).

## Why not cron?

cron skips any job whose time comes while your Mac is asleep, and tells you nothing when a job
fails. launchd, which macOS ships, runs a missed job when the Mac wakes, but it is configured with
XML files and `launchctl`. macron gives you cron's simplicity on top of launchd, plus a history of
every run.

## Contributing

See [`AGENTS.md`](AGENTS.md). It applies to people as much as to agents.
