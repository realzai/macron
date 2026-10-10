# JSON reference

Every command takes `--json`. With it, macron prints JSON on stdout instead of text, and an error
as JSON on stderr. This page describes what each command prints and every error code. The
options each command takes are in [cli.md](cli.md).

Put `--json` before the `--` that starts a command: after it, `--json` is part of the command.

```sh
macron add backup --json -s @daily -- ~/bin/backup.sh   # JSON
macron add backup -s @daily -- ~/bin/backup.sh --json   # runs `~/bin/backup.sh --json`
```

## Conventions

- **One document.** A command prints one pretty-printed JSON document, except `logs --follow`
  and `up`, which keep running and print one compact JSON object per line.
- **Times** are RFC 3339 in the Mac's time zone, to the second: `2026-10-11T09:00:00+01:00`.
- **Durations** of runs are whole milliseconds, in fields ending `_ms`. A job's `timeout` is a
  string as macron writes durations, such as `30m` or `1h 30m`.
- **Absent values** are `null`, never left out.
- **New fields** can appear in any release, so ignore fields you do not know. The fields, values
  and codes on this page keep their meaning.

## Exit status

| Status | Meaning |
|---|---|
| `0` | It worked |
| `1` | It failed; stderr has the error |
| `2` | The command line did not parse; stderr has a `usage` error |

`macron run --wait` exits as the run did, following shell conventions: the command's own status,
128 plus the number of the signal that killed it, `124` when its timeout stopped it, and `1` when
it could not start or was lost. A failed run is not an error, so stderr stays empty and stdout has
the run.

## Errors

An error is printed on stderr, and nothing on stdout:

```json
{"error":{"code":"no_such_job","message":"there is no job named `nope`"}}
```

Match on `code`; `message` is for people and its wording can change.

| Code | When | What to do |
|---|---|---|
| `usage` | The command line did not parse. Exits with status 2 | Read `message`, which names what is missing or wrong, and see [cli.md](cli.md) |
| `no_home` | `HOME` is not set | Set it |
| `relative_path` | A path in the environment, such as `MACRON_CONFIG_DIR`, is not absolute | Make it absolute |
| `invalid_schedule` | A schedule does not parse, or never runs, such as `0 0 31 2 *` | Fix the schedule; `message` says which part is wrong |
| `invalid_duration` | A `--timeout` does not parse, is not positive, or is in months or years | Use a form such as `90s`, `30m`, `1h 30m` or `2d` |
| `invalid_job_name` | A name has characters other than lowercase letters, digits, `-` and `_`, or is too long | Choose another name |
| `invalid_job` | A job's setting is not valid, such as an empty command, a shell that is not an absolute path, or a `workdir` neither absolute nor under `~` | Fix the setting `message` names |
| `job_exists` | `add` was given a name already in use | Choose another name, or change that job with `edit` |
| `no_such_job` | No job has that name | `macron ls` lists them |
| `invalid_config` | `jobs.toml` does not parse | Fix the file `message` names, then `macron apply` |
| `io` | Reading or writing a file failed | `message` says what and where |
| `launchctl` | `launchctl` failed in a way macron does not expect | Run `macron doctor` |
| `spawn_failed` | `macron exec` could not start a job's command, such as when its shell does not exist | Run `macron doctor`; the run is recorded as failed with the reason in `error` |
| `confirmation_required` | `rm` was not given `--yes` and there is no terminal to confirm on | Pass `--yes` |
| `no_runs` | `logs` was asked for a job that has not run yet | `macron run <job>` runs it now |
| `no_such_run` | `logs --run` was given a run the job does not have | `macron runs <job>` lists them |
| `job_disabled` | `run` was asked for a disabled job | `macron enable <job>`, then run it |
| `not_loaded` | `run` was asked for a job launchd has not loaded | `macron apply`, then run it |
| `already_running` | `run` was asked for a job that is running; launchd does not start a job twice | Wait for it, or follow it with `macron logs <job> --follow` |
| `did_not_start` | launchd did not start the job within 10 seconds of `run` | Read the job's launchd log, which `macron show <job>` names, and run `macron doctor` |
| `invalid_log` | A run's log is damaged | Its run is still in `macron runs`; the log cannot be read |
| `database` | The run history could not be read or written | Run `macron doctor` |
| `history_too_new` | The run history was written by a newer macron | Upgrade macron |
| `not_found` | Only from the web UI: a page that does not exist | |
| `port_in_use` | `up --port` was given a port in use | Choose another, or leave `--port` out to use any free port |
| `already_up` | `up --rotate-token` while another `macron up` is serving | Stop that one first |
| `internal` | A bug in macron | Please report it, with `message` |

## Objects

These appear in the output of several commands.

### Job

A job with every setting filled in, defaults included, and where it stands.

```json
{
  "name": "backup",
  "schedule": "0 9 1 * *",
  "schedule_description": "at 09:00 on the 1st of every month",
  "command": "~/bin/backup.sh",
  "description": "Back up the photos",
  "workdir": "~",
  "shell": "/bin/zsh",
  "env": { "MODE": "full" },
  "timeout": "30m",
  "notify": "failure",
  "keep_runs": 100,
  "enabled": true,
  "state": "active",
  "next_run": "2026-11-01T09:00:00+00:00",
  "last_run": null
}
```

| Field | |
|---|---|
| `name` | The job's name, which is also how every command refers to it |
| `schedule` | As written: cron, a shorthand such as `@daily`, or `@every 15m` |
| `schedule_description` | The schedule in words |
| `command` | The shell command line it runs |
| `description` | What it is for, or `null` |
| `workdir` | The directory it runs in, as written: absolute, or under `~` |
| `shell` | The shell that runs `command`, as a login shell |
| `env` | Environment variables set for the command, as an object |
| `timeout` | How long a run may take before it is stopped, or `null` for no limit |
| `notify` | When a macOS notification is posted about a finished run: `never`, `failure` or `always` |
| `keep_runs` | How many runs are kept, with their output |
| `enabled` | Whether it is meant to be installed in launchd |
| `state` | Where it stands with launchd; see below |
| `next_run` | When it runs next, or `null` when it is disabled or its schedule is `@every`, which launchd counts from when it loaded the job |
| `last_run` | Its most recent [run](#run), or `null` |

`state` is one of:

| State | Meaning |
|---|---|
| `active` | Installed, up to date and loaded: launchd runs it on schedule |
| `not_installed` | Enabled, but not installed. `macron apply` installs it |
| `out_of_date` | Installed, but not as `jobs.toml` now has it. `macron apply` updates it |
| `not_loaded` | Installed and up to date, but launchd has not loaded it. `macron apply` loads it |
| `disabled` | Disabled and not installed |
| `disabled_but_installed` | Disabled, but still installed. `macron apply` removes it |

### Run

One run of a job, as recorded in the run history.

```json
{
  "id": 42,
  "job": "backup",
  "trigger": "schedule",
  "status": "failed",
  "started_at": "2026-10-01T09:00:00+01:00",
  "finished_at": "2026-10-01T09:00:03+01:00",
  "duration_ms": 3104,
  "exit_code": 3,
  "signal": null,
  "error": null,
  "command": "~/bin/backup.sh",
  "log_bytes": 1830,
  "log_truncated": false
}
```

| Field | |
|---|---|
| `id` | The run's number, unique across every job's runs in the history |
| `job` | The job's name |
| `trigger` | What started it: `schedule` for launchd on the job's schedule, or `manual` for `macron run` and the web UI |
| `status` | `running`, `succeeded`, `failed`, `timed_out` or `lost`; see below |
| `started_at` | When it started |
| `finished_at` | When it finished, or `null` while it runs and when it was lost |
| `duration_ms` | How long it took, or `null` while it runs and when it was lost |
| `exit_code` | The command's exit status, or `null` when it did not exit by itself |
| `signal` | The number of the signal that killed the command, or `null` |
| `error` | Why macron failed the run itself, when the command could not start or the run was lost, or `null` |
| `command` | The command as it was when the run started; the job may have changed since |
| `log_bytes` | How much output the command printed and macron recorded, in bytes of text, not counting line breaks |
| `log_truncated` | Whether output was dropped because it passed 10 MiB |

`status` is one of:

| Status | Meaning |
|---|---|
| `running` | Still running |
| `succeeded` | The command exited with status 0 |
| `failed` | The command exited with another status, was killed by a signal, or could not start |
| `timed_out` | The job's timeout stopped it |
| `lost` | It never finished recording: the Mac shut down or restarted during it, or `macron exec` was killed |

### Change

What a command did, or would do, to launchd for one job.

```json
{ "action": "install", "job": "backup", "label": "io.github.realzai.macron.backup" }
```

| Field | |
|---|---|
| `action` | `install` writes the job's plist and loads it; `update` rewrites and reloads it; `load` loads a plist that is right but not loaded; `remove` unloads it and deletes the plist |
| `job` | The job's name |
| `label` | The job's launchd label |

### Line

One line of a run's output.

```json
{ "time": "2026-10-01T09:00:01+01:00", "stream": "err", "text": "oops" }
```

| Field | |
|---|---|
| `time` | When it was printed |
| `stream` | `out` and `err` for the command's stdout and stderr; `macron` for what macron noted about the run, such as a timeout |
| `text` | The line, without its newline. A line longer than 64 KiB arrives as several |

## Commands

### `macron ls`

```json
{ "jobs": [ /* Job, … */ ], "in_sync": true }
```

| Field | |
|---|---|
| `jobs` | Every [job](#job), in name order |
| `in_sync` | Whether launchd matches `jobs.toml`. When it is `false`, `macron apply` brings it in line |

### `macron show <job>`

The [job](#job), with these added:

| Field | |
|---|---|
| `next_runs` | Its next five runs, as times. Empty when `next_run` is `null` |
| `label` | Its launchd label |
| `plist` | Where its plist is, whether or not it is installed |
| `launchd_log` | Where launchd writes what macron could not record, such as a command that could not start |

### `macron add`, `edit`, `enable` and `disable`

```json
{ "job": { /* Job */ }, "changes": [ /* Change, … */ ] }
```

| Field | |
|---|---|
| `job` | The [job](#job) after the change |
| `changes` | The [changes](#change) made to launchd for it. Empty when launchd needed none, as when editing a disabled job |

### `macron rm <job>`

```json
{ "removed": "backup", "purged_runs": 0, "changes": [ /* Change, … */ ] }
```

| Field | |
|---|---|
| `removed` | The job's name |
| `purged_runs` | How many runs `--purge` deleted, with their output. Without `--purge` it is `0` and the runs are kept |
| `changes` | The [changes](#change) made to launchd |

### `macron apply`

```json
{ "dry_run": false, "changes": [ /* Change, … */ ] }
```

| Field | |
|---|---|
| `dry_run` | Whether `--dry-run` was given, so nothing was changed |
| `changes` | The [changes](#change) made, or with `--dry-run` the ones that would be. Empty when launchd already matches `jobs.toml` |

### `macron run <job>`

The [run](#run), just started: its `status` is usually `running`.

With `--wait`, the run once it has finished, printed then and not before. The command's output is
not printed; `macron logs <job> --run <id> --json` has it. The exit status is the run's, as above.

### `macron runs`

```json
{ "runs": [ /* Run, … */ ] }
```

`runs` is the [runs](#run), newest first, filtered by the job, `--status` and `--limit` given.

### `macron logs <job>`

```json
{ "run": { /* Run */ }, "lines": [ /* Line, … */ ] }
```

| Field | |
|---|---|
| `run` | The [run](#run): the job's latest, unless `--run` names one |
| `lines` | Its output, as [lines](#line) |

With `--follow`, one compact JSON object per line instead: each [line](#line) of output as it
arrives, from the start of the run, and last `{"run": { … }}` with the run once it has ended.

```json
{"time":"2026-10-01T09:00:01+01:00","stream":"out","text":"copying"}
{"time":"2026-10-01T09:00:03+01:00","stream":"err","text":"oops"}
{"run":{"id":42,"job":"backup","status":"failed", … }}
```

`--raw` has no effect with `--json`.

### `macron up`

One compact line once the web UI is being served, and then nothing more until it stops:

```json
{"url":"http://127.0.0.1:6220","sign_in_url":"http://127.0.0.1:6220/auth?token=…","pid":81312,"already_up":false}
```

| Field | |
|---|---|
| `url` | Where the web UI is |
| `sign_in_url` | A link that signs a browser in. It holds the sign-in secret, so treat it as a password |
| `pid` | The `macron up` process serving it. SIGINT or SIGTERM stops it |
| `already_up` | Whether another `macron up` was already serving it. If so, this one exits at once |

### `macron doctor`

```json
{
  "healthy": false,
  "checks": [
    {
      "check": "jobs",
      "severity": "problem",
      "message": "`backup` runs in /nonexistent/fish, which is not an executable file",
      "fix": "Use a shell that exists: `macron edit backup --shell /bin/zsh`."
    }
  ],
  "notes": ["If a job fails to read ~/Documents, …"]
}
```

| Field | |
|---|---|
| `healthy` | Whether no check found a problem. Warnings do not count |
| `checks` | What each check found, in order. A check can appear more than once, once for each thing it found |
| `checks[].check` | What was checked: `config`, `launchd`, `jobs`, `binary`, `history` or `launchd_log` |
| `checks[].severity` | `ok`; `warning`, for something that deserves a look; or `problem`, for something that stops a job running as configured |
| `checks[].message` | What it found |
| `checks[].fix` | What to do about it, or `null` when the severity is `ok` |
| `notes` | What is worth knowing but cannot be checked |

`doctor` exits with status 0 whatever it finds; read `healthy`.

### `macron paths`

```json
{
  "config_dir": "/Users/ada/.config/macron",
  "jobs_file": "/Users/ada/.config/macron/jobs.toml",
  "state_dir": "/Users/ada/.local/state/macron",
  "history_db": "/Users/ada/.local/state/macron/history.db",
  "logs_dir": "/Users/ada/.local/state/macron/logs",
  "launchd_logs_dir": "/Users/ada/.local/state/macron/launchd",
  "launch_agents_dir": "/Users/ada/Library/LaunchAgents"
}
```

| Field | |
|---|---|
| `config_dir` | Where macron's configuration is |
| `jobs_file` | `jobs.toml`, which holds every job |
| `state_dir` | Where macron keeps what it records |
| `history_db` | The run history, an SQLite database |
| `logs_dir` | Each run's output, one file per run |
| `launchd_logs_dir` | Each job's launchd log |
| `launch_agents_dir` | Where macron installs jobs' plists for launchd |

`MACRON_CONFIG_DIR` and `MACRON_STATE_DIR` move the first and third, and without them
`XDG_CONFIG_HOME` and `XDG_STATE_HOME` are followed.
