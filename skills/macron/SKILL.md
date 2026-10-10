---
name: macron
description: Schedule, run and debug recurring jobs on the user's Mac with the macron CLI, which installs them into launchd and records every run with its output. Use when the user wants something to run on a schedule on their Mac (every day, every Monday, the 1st of the month, every 15 minutes), asks why a scheduled job failed or did not run, wants to see a job's output or history, or wants to change, pause, run now or remove a scheduled job.
---

# macron

macron keeps the user's scheduled jobs in `~/.config/macron/jobs.toml`, installs each enabled one
into launchd, and records every run with its output. You drive it with the `macron` CLI. Unlike
cron, launchd runs a job missed while the Mac slept, once, when it wakes.

## Before you start

- Run `macron --version`. If it is not found, ask the user before installing it:
  `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/realzai/macron/releases/latest/download/macron-cli-installer.sh | sh`,
  or with Rust installed, `cargo install macron-cli`.
- **Always pass `--json`, before any `--`.** Everything after `--` is the job's command, so
  `macron add x -s @daily -- ./sync --json` adds a job that runs `./sync --json`.
- **Single-quote the job's command and any `~` or `$VAR` in it**, so they reach the job as
  written and are expanded when it runs, in the user's environment, not by your shell now. macron
  expands a leading `~` in `--workdir` itself.
- Output is one JSON document on stdout. An error goes to stderr as
  `{"error":{"code":"…","message":"…"}}` with exit status 1, or 2 for a command line that did not
  parse. Match on `code`, read `message`.
- `macron run --wait` is the exception: it exits with the job's own exit status, so a job that
  exits 1 or 2 looks like a macron error by status alone. Tell them apart by the output: a Run on
  stdout, or an error on stderr.
- A job runs unattended on the user's Mac. Before adding one or changing what it runs, be sure the
  user wants that command run on that schedule. Ask before `rm`, and before `--purge`, which
  deletes the job's run history.
- Use macron, not `launchctl` or the plists in `~/Library/LaunchAgents`. macron rewrites what it
  owns and would undo your change.

## Commands

| Command | Does | Prints |
|---|---|---|
| `macron ls --json` | Every job and where it stands; `in_sync` `false` means run `macron apply --json` | `{"jobs": [Job], "in_sync": bool}` |
| `macron show <job> --json` | One job, with its next five runs and file paths | Job + `next_runs`, `label`, `plist`, `launchd_log` |
| `macron add <job> --json -s <schedule> [options] -- <command>` | Adds a job and installs it | `{"job": Job, "changes": [...]}` |
| `macron edit <job> --json [options] [-- <command>]` | Changes a job and reinstalls it | same as `add` |
| `macron disable <job> --json` / `enable` | Pauses or resumes a job, keeping it | same as `add` |
| `macron rm <job> --yes --json [--purge]` | Removes a job; `--purge` also deletes its runs | `{"removed", "purged_runs", "changes"}` |
| `macron run <job> --wait --json` | Runs it now, through launchd, and waits | the finished Run; exits with the job's exit status |
| `macron runs [<job>] --json [--status failed] [--limit N]` | Recent runs, newest first | `{"runs": [Run]}` |
| `macron logs <job> --json [--run <id>]` | A run's output, the latest by default | `{"run": Run, "lines": [{"time", "stream", "text"}]}` |
| `macron doctor --json` | Checks everything that stops jobs running | `{"healthy", "checks": [...], "notes": [...]}` |
| `macron apply --json` | Makes launchd match `jobs.toml` after a hand edit | `{"dry_run", "changes": [...]}` |
| `macron paths --json` | Where jobs, history and logs live | paths |
| `macron up --no-open --json` | Serves the web UI for the user; prints `sign_in_url` | one line, then keeps running |

`changes` lists what was done to launchd: `install`, `update`, `load` or `remove`. launchd holds
only when a job runs, so a new schedule, `disable` or `enable` changes it. The command and every
other setting are read each time the job runs, so changing only those gives `"changes": []` and
takes effect from the next run.

Options for `add` and `edit`: `-d <description>`, `--workdir <dir>` (default `~`),
`--shell <path>` (default `/bin/zsh`), `-e KEY=VALUE` (repeatable; the value is used as written,
with no `~` or `$VAR` expansion), `--timeout <duration>` (`30m`, `1h 30m`),
`--notify never|failure|always` (a macOS notification; default `failure`), `--keep-runs <N>`
(default 100), `--disabled` (`add` only). `edit --unset <field>` puts a field back to its default.

Job names are lowercase letters, digits, `-` and `_`.

**Job** fields: `name`, `schedule`, `schedule_description`, `command`, `description`, `workdir`,
`shell`, `env`, `timeout`, `notify`, `keep_runs`, `enabled`, `state`, `next_run`, `last_run` (a
Run or `null`). `state` is `active` when launchd will run it; anything else (`not_installed`,
`out_of_date`, `not_loaded`, `disabled_but_installed`) is fixed by `macron apply`, except
`disabled`, which is on purpose.

**Run** fields: `id`, `job`, `trigger` (`schedule` or `manual`), `status`, `started_at`,
`finished_at`, `duration_ms`, `exit_code`, `signal`, `error`, `command`, `log_bytes`,
`log_truncated`. `id` is numbered across all jobs, not per job: one job's runs can be 1, 3, 5.
`status` is `running`, `succeeded`, `failed`, `timed_out` or `lost`. In a log
line, `stream` is `out`, `err`, or `macron` for macron's own notes, such as a timeout.

## Schedules

Times are in the Mac's time zone.

| Want | Schedule |
|---|---|
| Every day at 09:00 | `0 9 * * *` |
| Weekdays at 08:30 | `30 8 * * MON-FRI` |
| Every Monday at 09:00 | `0 9 * * MON` |
| The 1st of every month at 09:00 | `0 9 1 * *` |
| Every 15 minutes, on the quarter hour | `*/15 * * * *` |
| Every 15 minutes, counted from when it was installed | `@every 15m` |
| Every hour, on the hour | `@hourly` |
| Midnight daily, Sunday midnight, the 1st at midnight, 1 January | `@daily`, `@weekly`, `@monthly`, `@yearly` |

Cron is `minute hour day-of-month month day-of-week`, with `*`, lists (`1,15`), ranges (`1-5`),
steps (`*/10`, `0-30/5`) and names (`MON`, `JAN`). When both day-of-month and day-of-week are
restricted, the job runs on days matching **either**, as in cron. `@every` takes `10s` to weeks,
not months.

When the user says "every week" or "monthly" without a time, pick a sensible daytime time rather
than `@weekly`'s midnight, and say which you picked. After adding or editing, read
`schedule_description` and, from `macron show <job> --json`, `next_runs`, and check they say what
the user asked for.

## Add a job

1. Choose a name, a schedule and the command. Use absolute paths or `~/...` for scripts.
2. `macron add <name> --json -s '<schedule>' -d '<what it is for>' -- '<command>'`. Quote the
   command as one argument when it has `&&`, `|`, `;` or redirects.
3. Check `job.state` is `active` and `job.schedule_description` matches the request.
4. If running it now is harmless and the user agrees, `macron run <name> --wait --json` proves it
   works where it will really run. Then read its output with `macron logs <name> --json`.

## Diagnose a failing job

1. `macron ls --json`: find the job. Note `state` and `last_run`.
   - `state` is not `active` and not `disabled`: run `macron doctor --json`, then `macron apply --json`.
2. `macron runs <job> --json --limit 10`: look for a pattern. Always failing? Since when? Only on
   schedule (`trigger: schedule`) and not by hand?
3. `macron logs <job> --json --run <id>` for a failed run. Read the `err` and `macron` lines.
4. If the log does not explain it, `macron doctor --json`: every check with `severity` `problem`
   or `warning` has a `fix`. It checks the machine and each job's shell and workdir, not whether
   the programs a command calls exist.
5. Match the evidence:

| Evidence | Cause | Fix |
|---|---|---|
| `exit_code` 127, "command not found" | The program is not on the job's `PATH`. Jobs run in a non-interactive login shell (`zsh -lc` by default), which reads `~/.zprofile` but not `~/.zshrc` | Find the program: `zsh -ic 'command -v <program>'` reads `~/.zshrc` as the user's terminal does, or look for `PATH` in `~/.zshrc`. Then prefer, in order: its absolute path in the command, which changes only this job; a full `-e PATH=/Users/<user>/tools:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`, written out because `$PATH` is not expanded; moving the user's `PATH` setup to `~/.zprofile`, which changes every job and shell (ask first) |
| `exit_code` 126 | The script is not executable | `chmod +x` it |
| "Operation not permitted" reading `~/Documents`, `~/Desktop`, `~/Downloads` or app data | macOS privacy protection. It can start after macron is upgraded or rebuilt, which can drop an earlier grant | Only the user can fix it: System Settings → Privacy & Security → Full Disk Access, add the macron binary named in `doctor`'s `notes` |
| `status` `timed_out` | It ran past `timeout` | Find why it hangs (often waiting for input or a network), or raise `--timeout` |
| `status` `lost` | The Mac shut down or restarted during the run | Usually not the job's fault; check the next run |
| `status` `failed`, `exit_code` `null`, `error` set | The command could not start: its shell or workdir is missing | `macron doctor --json` names it; `macron edit` to fix |
| `signal` 9 or 15 | Something killed it | Look for memory use or another process stopping it |
| Works in a terminal, fails as a job | The job has no terminal, no `~/.zshrc`, runs in `workdir` (`~` by default), and cannot answer prompts | Make the command non-interactive, `cd` or set `--workdir`, pass needed variables with `-e` |

6. Fix it with `macron edit`, then, if harmless, `macron run <job> --wait --json` and confirm
   `status` is `succeeded`.

### A job that did not run at all

- `next_run` in `macron show <job> --json` is not what was expected: the schedule is wrong.
- `state` is not `active`: `macron apply --json`.
- The Mac was off at that time. Missed runs during sleep run on wake; missed runs while powered
  off do not.
- `@every` counts from when launchd loaded the job (at install, and at each login), not from the
  clock, so `next_run` is `null` for it.
- The job's launchd log (`launchd_log` in `show`) holds what macron could not record, such as a
  run that could not start. `doctor` reports recent entries.
- macron was upgraded, moved or rebuilt, and installed jobs still run the old binary. `doctor`
  reports it under `binary`; `macron apply --json` fixes it.
- On the two nights a year the clocks change, a run inside the skipped or repeated hour runs
  when launchd decides, which can differ from `next_run`.

## Change, pause, run now, remove

- New schedule: `macron edit <job> --json -s '<schedule>'`.
- New command: `macron edit <job> --json -- '<command>'`.
- Pause and resume: `macron disable <job> --json`, `macron enable <job> --json`.
- Run now: `macron run <job> --wait --json`. Without `--wait`, it returns at once with the run
  `running`; follow it with `macron logs <job> --json --run <id>` later.
- Remove, after the user agrees: `macron rm <job> --yes --json`. Add `--purge` only if they want
  the history gone too.

## Errors you will meet

| `code` | Meaning | Do |
|---|---|---|
| `usage` | The command line did not parse | Read `message`; it names what is missing |
| `no_such_job` | No job by that name | `macron ls --json` |
| `job_exists` | `add` with a name in use | `edit` that job, or choose another name |
| `invalid_job_name` | Not lowercase letters, digits, `-`, `_` | Rename |
| `invalid_schedule` | Does not parse, never runs (`0 0 30 2 *`), or an `@every` under 10s or in months | `message` says what is wrong |
| `invalid_duration` | A bad `--timeout` | Use `90s`, `30m`, `1h 30m`, `2d` |
| `invalid_job` | A bad setting, such as a relative `--shell` | Fix what `message` names |
| `invalid_config` | `jobs.toml` does not parse | Fix the file, then `macron apply --json` |
| `confirmation_required` | `rm` without `--yes` | Confirm with the user, then pass `--yes` |
| `job_disabled` | `run` on a disabled job | `enable` first, if the user wants it running |
| `not_loaded` | `run` on a job launchd has not loaded | `macron apply --json` |
| `already_running` | launchd does not start a job twice | Wait; `macron logs <job> --json` shows progress |
| `did_not_start` | launchd did not start it within 10 seconds | `macron doctor --json`, and read the job's `launchd_log` |
| `no_runs` | `logs` for a job that has never run | Nothing to read yet |
| `no_such_run` | `logs --run <id>` with an id that is not one of this job's runs | `macron runs <job> --json` lists its ids |
| `launchctl`, `database`, `io` | Something on the machine failed | `macron doctor --json` |

The full reference, every command's options and every field and code, is in
[docs/cli.md](https://github.com/realzai/macron/blob/main/docs/cli.md) and
[docs/json.md](https://github.com/realzai/macron/blob/main/docs/json.md).

## Hand edits

Jobs live in `jobs.toml` (`macron paths --json` → `jobs_file`), which the user may edit. After
editing it, run `macron apply --json`. Prefer `add` and `edit`, which validate and apply in one
step and keep the file's comments.
