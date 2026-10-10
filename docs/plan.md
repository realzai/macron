# macron: plan

macron manages scheduled jobs on a Mac. You describe jobs once, macron installs them into launchd,
records every run, and lets you browse the history and logs from the CLI (`macron logs`) or the
browser (`macron up`). Agents get the same through `--json` and a skill.

This is the build plan. It is expected to go stale in its milestones; the *Decisions* and *How it
works* sections should stay true, and a change to either is a change to this file first.

## Goals

1. `macron up` opens a local web interface where every job can be created, edited, enabled,
   disabled, run now and deleted.
2. Every run is recorded (when, why, how long, exit status) with its output, and both are browsable
   from the CLI and from the web.
3. Agents can drive it without a human: every command has a stable `--json` form and documented
   error codes, nothing prompts when there is no terminal, and the repo ships a skill.
4. Open source on GitHub, installable with Homebrew or `cargo install`.

## Non-goals, for now

- **Linux or Windows.** The name is a promise. A `Scheduler` boundary keeps the door open; nothing
  else is done for it.
- **Running jobs missed while the Mac was off.** launchd runs a job missed during sleep once, on
  wake. Power-off misses are lost. Catch-up is a possible later feature, not v1.
- **Remote access or multiple users.** The web UI is for the person at the keyboard, on loopback.
- **System-wide jobs (LaunchDaemons, root).** Per-user LaunchAgents only.

## Decisions

| Decision | Choice | Why |
|---|---|---|
| Scheduler | launchd, per-user LaunchAgents in the `gui/<uid>` domain | Runs missed-during-sleep jobs on wake, which cron does not. No root needed |
| Source of truth | `jobs.toml` in the config dir | Diffable, dotfile-friendly, and an agent can edit it directly. Plists are generated output, never edited by hand |
| What launchd runs | Always `macron exec <job>`, never the job's command directly | One place captures output, records history, enforces timeouts and notifies. macron never has to parse `launchctl` output |
| Run history | SQLite (`rusqlite`, bundled), WAL mode | Many writers (one `exec` per run) and readers (CLI, web) at once |
| Run output | One JSON Lines file per run, each line tagged with its stream and time | Keeps stdout and stderr interleaved in order. The CLI, the web and agents all read the same file |
| Web UI | `axum` serving server-rendered `askama` templates, `htmx` for interaction, assets embedded with `rust-embed` | No Node toolchain, so `cargo install` builds everything, and the binary is the whole product |
| Time | `jiff` | Correct next-run times across time zones and DST |
| Packaging | One crate, `macron-cli`, with a library and a `macron` binary | `macron` is taken on crates.io. One crate keeps `cargo install` and publishing simple. Split only if a second consumer of the library appears |
| launchd labels | `io.github.realzai.macron.<job>` | Reverse-DNS, and the prefix is how macron finds its own plists. It never touches a plist without it. The repo stays under `realzai`, so the prefix is permanent: changing it later would mean migrating every user's installed jobs |
| License | MIT | |
| Commits | Conventional Commits | The changelog is generated from them at release |

## How it works

```
jobs.toml ──apply──▶ ~/Library/LaunchAgents/io.github.realzai.macron.<job>.plist
                              │  launchctl bootstrap
                              ▼
                           launchd ──at the scheduled time──▶ macron exec <job>
                                                                 │ runs: <shell> -lc "<command>"
                                                                 ├─▶ history.db   (one row per run)
                                                                 └─▶ logs/<job>/<run-id>.jsonl
macron ls / runs / logs ◀── reads ──┘                          and macron up (web) reads the same
```

- **Every write goes through one path.** `macron add`, an edit in the web UI and a hand edit of
  `jobs.toml` followed by `macron apply` all end in the same reconcile: generate the plists the
  config implies, compare them with what is installed, bootstrap or boot out the difference.
- **A disabled job has no plist.** launchd loads any plist in `LaunchAgents` at login, so disabling
  removes it. macron never uses `launchctl enable/disable`, whose overrides persist out of sight.
- **`apply` only touches labels with macron's prefix.** A macron plist with no job in the config is
  an orphan and is removed. Nothing else in `LaunchAgents` is read or changed.
- **Each plist carries `MACRON_CONFIG_DIR` and `MACRON_STATE_DIR`**, so `exec` reads and writes the
  same files `apply` used, whatever the launchd environment looks like.
- **launchd's own stdout and stderr go to `state/launchd/<job>.log`.** Once `exec` records runs, that
  file only ever holds failures of macron itself, such as a binary that cannot start, which would
  otherwise vanish.

### Paths

| What | Default | Override |
|---|---|---|
| Config dir (`jobs.toml`) | `$XDG_CONFIG_HOME/macron`, else `~/.config/macron` | `MACRON_CONFIG_DIR` |
| State dir (`history.db`, `logs/`) | `$XDG_STATE_HOME/macron`, else `~/.local/state/macron` | `MACRON_STATE_DIR` |
| Plists | `~/Library/LaunchAgents` | `MACRON_LAUNCH_AGENTS_DIR` |

`macron paths` prints all of them. The overrides exist so tests never touch the real locations.

### `jobs.toml`

```toml
[jobs.backup]
schedule = "0 9 1 * *"            # 09:00 on the 1st of every month
command = "~/bin/backup.sh"
description = "Monthly photo backup"

[jobs.inbox-sweep]
schedule = "@every 15m"
command = "deno run -A ~/scripts/sweep.ts"
workdir = "~/scripts"
timeout = "5m"
env = { SWEEP_DRY_RUN = "0" }
notify = "always"                 # never | failure (default) | always
enabled = false
```

| Field | Default | Notes |
|---|---|---|
| `schedule` | required | Five-field cron, `@hourly` `@daily` `@weekly` `@monthly` `@yearly`, or `@every <duration>` |
| `command` | required | Run as `<shell> -lc "<command>"`, so the login profile (and Homebrew's `PATH`) is loaded |
| `shell` | `/bin/zsh` | |
| `workdir` | `~` | |
| `env` | none | Added to the job's environment |
| `timeout` | none | `jiff` friendly durations: `90s`, `5m`, `1h 30m`. On expiry the process group gets SIGTERM, then SIGKILL after 10 s |
| `notify` | `failure` | macOS notification on run end |
| `keep_runs` | `100` | Older runs and their logs are pruned at the end of each run |
| `enabled` | `true` | |
| `description` | none | Shown in `ls` and the web UI |

Job names match `[a-z0-9][a-z0-9_-]{0,63}`, because they become part of labels and paths. Writes
use `toml_edit`, so comments and formatting in a hand-edited file survive an edit from the CLI or
the web, and take an exclusive `File::lock` on the file for the read-modify-write.

### Schedules

macron parses schedules itself and owns the conversion, because the conversion is the product:

- **Cron to launchd.** `StartCalendarInterval` has no ranges or steps, so each restricted field is
  expanded and the result is the cartesian product, one dict per combination. A field that allows
  every value is omitted, which launchd reads as "any". `*/15 * * * *` becomes 4 dicts.
  `*/5 9-17 * * 1-5` becomes 540; that is fine, and a schedule needing more than 1,000 is refused
  when it is parsed.
- **Day-of-month and day-of-week.** Cron runs on days matching either field when both are
  restricted. macron never puts `Day` and `Weekday` in the same dict, so it does not depend on how
  launchd combines them: both restricted means two dict sets, one per field, whose union is cron's
  "either". A field counts as restricted unless it is exactly `*`.
- **Refused at parse time:** schedules that never run (`0 0 30 2 *`), `@every` under 10 seconds
  (launchd's minimum), and the cron extensions launchd cannot express (`L`, `W`, `#`, `?`).
- **`@every`** becomes `StartInterval`. launchd counts it from when the job was loaded, not from a
  wall-clock boundary, and the docs must say so.
- **Describing and predicting.** The same parse produces the plain-English description ("at 09:00
  on the 1st of every month") and the next run times shown in `ls` and the web UI, computed in local
  time with `jiff`.

The parser is checked against `croner` as an oracle in property tests: for random expressions, the
next run times must agree.

### Runs

`macron exec <job>` is what launchd starts. It:

1. Inserts a `runs` row: job, trigger (`schedule` or `manual`), start time, pid, command snapshot,
   status `running`.
2. Starts `<shell> -lc "<command>"` in its own process group, with `workdir` and `env` applied.
3. Reads stdout and stderr as lines into `logs/<job>/<run-id>.jsonl`, each line
   `{"t":"<RFC 3339>","s":"out"|"err","l":"<text>"}`. Invalid UTF-8 is replaced, not dropped. A run
   stops recording past 10 MiB and marks its log truncated.
4. Enforces `timeout`, killing the group with `nix::sys::signal::killpg`.
5. Updates the row: end time, duration, exit code or signal, status `succeeded`, `failed` or
   `timed_out`.
6. Notifies per `notify`, then prunes runs beyond `keep_runs`.

A `running` row whose pid is gone (the Mac shut down mid-run) is marked `lost` the next time
anything reads history.

**Manual runs go through launchd too.** `macron run <job>` writes a trigger marker into the state
dir and calls `launchctl kickstart`, so a manual run gets exactly the environment a scheduled run
gets. "It works when I run it but fails on schedule" is the most common cron complaint, and this
removes it. `exec` consumes the marker to record the trigger as `manual`. launchd never starts a
second instance of a running label, so runs of one job cannot overlap.

The schema is versioned with `PRAGMA user_version` and migrated forward on open.

## CLI

Every command takes `--json` and prints one JSON document to stdout. Errors go to stderr as
`{"error":{"code":"…","message":"…"}}`. Exit status is 0 for success, 1 for an error, 2 for a usage
error. Error codes are a contract and live in `src/error.rs`. Nothing prompts without a terminal:
`rm` asks on a TTY and otherwise needs `--yes`.

| Command | Does |
|---|---|
| `macron up [--port N] [--no-open]` | Starts the web UI and opens the browser. If one is already running, opens that instead |
| `macron ls` | Jobs: schedule, its description, next run, last run status and time, enabled |
| `macron show <job>` | One job in full, with its generated launchd schedule |
| `macron add <job> --schedule <s> [options] -- <command…>` | Adds a job and installs it |
| `macron edit <job> [--schedule <s>] [options] [-- <command…>]` | Changes fields of a job and reinstalls it |
| `macron rm <job> [--yes]` | Uninstalls a job and removes it from `jobs.toml`. History stays until `--purge` |
| `macron enable <job>` · `macron disable <job>` | |
| `macron run <job> [--wait]` | Runs it now, through launchd. `--wait` follows the log until it ends and exits with the run's status |
| `macron runs [<job>] [--limit N] [--status failed]` | Run history, newest first |
| `macron logs <job> [--run <id>] [--follow] [--raw]` | A run's output, the latest by default. stderr is highlighted; `--raw` prints the lines with no decoration |
| `macron apply [--dry-run]` | Reconciles launchd with `jobs.toml`. `--dry-run` prints the plan |
| `macron doctor` | Checks config, plists, loaded state, binary path, and Full Disk Access hints |
| `macron paths` | Where everything is |
| `macron exec <job>` | Internal: what launchd runs. Hidden from help |

`launchctl` is only ever judged by its exit status (`launchctl print gui/<uid>/<label>` exits 0 when
the job is loaded); its output format is undocumented and changes between releases. The binary is
`MACRON_LAUNCHCTL` if set, so tests substitute a fake that records its arguments.

## Web UI: `macron up`

`macron up` serves on `127.0.0.1` at a fixed default port, so a bookmark keeps working, and falls
back to a free port if it is taken. It runs in the foreground until Ctrl-C. It writes
`state/up.json` (pid, port); a second `macron up` that finds a live instance just opens it.

| Page | Shows |
|---|---|
| `/` | Every job: schedule and description, next run, last run status, enabled toggle, Run now. Refreshes itself |
| `/jobs/new`, `/jobs/<job>` | Create and edit forms with the schedule described as you type and inline errors; the job's run history |
| `/jobs/<job>/runs/<id>` | One run's log. stderr is highlighted. While the run is live, new lines stream in over SSE |
| `/runs` | Recent runs across all jobs, failures first if filtered |

Handlers call the same library functions as the CLI. There is no logic in a handler that the CLI
does not also have.

**Security.** A page that can create jobs can run any command as you, so any website you visit
must not be able to reach it:

- Bind to loopback only.
- Reject any request whose `Host` is not `127.0.0.1:<port>` or `localhost:<port>`. This stops DNS
  rebinding.
- Require a session cookie. `macron up` opens `/auth?token=<secret>`, which sets an `HttpOnly`,
  `SameSite=Strict` cookie and redirects. The secret lives in the state dir with mode 0600 and
  `macron up --rotate-token` replaces it.
- Reject a state-changing request whose `Origin` is not the server's own.

These are tested, not just implemented.

## Agent support

- `--json` and error codes on every command, from the first milestone, not added at the end.
- `skills/macron/SKILL.md`: when to reach for macron, the commands, and recipes for the jobs agents
  get asked to do (add a job, find out why a job failed, change a schedule), plus the traps below.
  Installable with `npx skills add realzai/macron`.
- `docs/cli.md` is generated from the clap definitions, and CI fails if it is out of date, so the
  reference cannot drift from the binary.
- `docs/json.md` documents each command's JSON shape and every error code.
- `AGENTS.md` is for agents working *on* macron; the skill is for agents *using* it.

## Testing

- **Unit:** schedule parsing, expansion, description and next-run (property tests against `croner`);
  plist generation (compared with literal XML); `jobs.toml` edits preserve comments; path
  resolution.
- **Integration:** the built binary, run with `MACRON_CONFIG_DIR`, `MACRON_STATE_DIR` and
  `MACRON_LAUNCH_AGENTS_DIR` in a temp dir and `MACRON_LAUNCHCTL` pointing at a fake. `exec` runs
  real short commands (`echo hi; exit 3`) and the tests read the history row and the log back.
- **Web:** the `axum` router in-process with `tower::ServiceExt::oneshot`, including every security
  rule above.
- **End to end:** `#[ignore]`d tests that really load a job into launchd, kickstart it and wait for
  its run row. Run with `cargo test -- --ignored` on a real Mac before each release. Whether GitHub's
  macOS runners have a usable `gui/<uid>` domain is unverified.
- Tests never call `std::env::set_var` (it is `unsafe` in edition 2024 and the crate forbids
  `unsafe`). Environment is passed to a child process or through a lookup closure, as
  `Paths::resolve` does.

## Milestones

Each one ends with something usable and its tests passing in CI.

**M0. Scaffold (done).** The crate, the dependencies, `macron paths`, the `--json` and error
conventions with their tests, CI, `AGENTS.md`, this plan.

**M1. Jobs reach launchd (done).** `jobs.toml` load and locked save; the schedule module (parse, expand,
describe, next run); plist generation; the `launchctl` wrapper; `add`, `edit`, `rm`, `ls`, `show`,
`enable`, `disable`, `apply --dry-run`; a first `exec` that runs the job's command with its shell,
working directory, environment and timeout, its output going to launchd's log until M2 records it.
*Done when* a job added with `macron add` fires on schedule on a real Mac.

**M2. Runs and logs.** `exec`, the history database and its migrations, JSONL logs, truncation,
timeouts, trigger markers, `lost` detection, notifications, pruning; `run --wait`, `runs`,
`logs --follow`. *Done when* a failing job shows in `macron runs` with its exit status and its
stderr in `macron logs`.

**M3. Web UI.** `macron up` with the security rules; the pages above; live log streaming; create,
edit, enable, disable, delete and run now. *Done when* everything from M1 and M2 can be done in the
browser.

**M4. Agents and docs.** The skill, the generated CLI reference, the JSON reference, `doctor`, a
README with screenshots. *Done when* an agent given only the skill can add a job and diagnose a
failing one.

**M5. Release.** `cargo-dist` for GitHub Releases and a Homebrew tap (`realzai/homebrew-tap`),
crates.io publishing of `macron-cli`, a changelog, an MSRV check in CI, v0.1.0.

## Traps

These are why the design looks the way it does. Each one belongs in the skill and the README too.

- **Sleep versus power-off.** launchd runs a job missed while the Mac slept, once, however many runs
  were missed. Missed while off, it never runs.
- **Full Disk Access is tied to the binary's code signature.** Granting it to `macron` covers every
  job, because launchd starts `macron` and the job is its child. But an ad-hoc signed binary gets a
  new signature on every build or upgrade and silently loses the grant. `doctor` explains this;
  release builds should be signed with a stable identity.
- **The binary path in the plists must be stable.** A Homebrew upgrade moves the `Cellar` path, so
  plists point at the `bin/` symlink, not the resolved file. `apply` rewrites every plist when the
  path changes and `doctor` flags plists pointing at a missing binary.
- **launchd's environment is nearly empty.** That is why commands run under a login shell.
- **launchd will not start a running job again.** `kickstart` on a running job does nothing. `macron
  run` reports that instead of appearing to work.
- **DST.** On the night the clocks change, a job scheduled inside the skipped or repeated hour runs
  when launchd decides. The predicted next run can disagree on those two nights; say so in the UI.
