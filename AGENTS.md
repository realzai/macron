# AGENTS.md

For anyone, human or agent, working **on** macron. An agent that wants to **use** macron to
schedule jobs wants [`skills/macron/SKILL.md`](skills/macron/SKILL.md) instead.

Read [`docs/plan.md`](docs/plan.md) before changing anything. It holds the decisions, how the
pieces fit, and which milestone is next. If a change contradicts the plan, change the plan in the
same commit and say why.

## Commands

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo run -- paths            # run the binary; arguments go after --
MACRON_UPDATE_DOCS=1 cargo test --lib cli_reference   # regenerate docs/cli.md
```

CI runs `fmt --check`, `clippy -D warnings` and `test` on macOS. Run all three before calling a
change done.

## Layout

| Path | What |
|---|---|
| `src/main.rs` | Calls `cli::run`. Nothing else goes here |
| `src/lib.rs` | The library. The CLI and the web UI are both clients of it |
| `src/ops.rs` | Every operation macron offers, through `Macron`. The CLI and the web UI both call it |
| `src/cli/` | Argument parsing and rendering only. No logic a web handler would also need |
| `src/web/` | The web UI: the server and its security rules (`security.rs` explains them), and the pages |
| `templates/` | The web UI's pages, as askama templates. Escaping is automatic; `\|safe` only for HTML macron rendered itself |
| `assets/` | The web UI's stylesheet and script, embedded in the binary. No inline scripts or styles: the content security policy forbids them |
| `src/human.rs` | Times, durations and run statuses in words, shared by the CLI and the web UI |
| `src/schedule/` | Parsing schedules, converting them for launchd, describing them, predicting runs |
| `src/job.rs` | A job and its validation |
| `src/config.rs` | Reading `jobs.toml`, and editing it without losing comments |
| `src/launchd.rs` | Plists, `launchctl`, and reconciling launchd with `jobs.toml` |
| `src/exec.rs` | Running a job: the command, its output, its timeout, and recording the run |
| `src/history.rs` | The run history database |
| `src/runlog.rs` | Per-run output logs |
| `src/notify.rs` | macOS notifications about finished runs |
| `src/time.rs` | Times as macron writes them for machines |
| `src/duration.rs` | Durations as people write them (`15m`, `1h 30m`) |
| `src/error.rs` | Every error and its stable code |
| `src/doctor.rs` | `macron doctor`'s checks |
| `src/paths.rs` | Where config, state and plists live, and the environment overrides |
| `tests/` | The built binary end to end, against a sandbox with stand-ins for `launchctl` and `osascript` (`tests/support/`) |
| `docs/plan.md` | The plan |
| `docs/cli.md` | The command-line reference, generated from `src/cli` |
| `docs/json.md` | What every command prints with `--json`, and every error code |
| `skills/macron/` | The skill that teaches agents to use macron |

## Rules

- **One code path for the CLI and the web.** Logic lives in the library, and operations in
  `Macron` (`src/ops.rs`). A CLI command and a web handler that do the same thing call the same
  method.
- **Every command supports `--json`.** One JSON document on stdout. Errors on stderr as
  `{"error":{"code":"…","message":"…"}}`. Exit 0 for success, 1 for an error, 2 for a usage error.
- **Error codes are a contract.** Agents match on `Error::code`. Never change what a released code
  means; add a new variant, and document it in `docs/json.md`. `tests/docs.rs` fails until you do.
- **The docs follow the code.** A change to a command or its options regenerates `docs/cli.md`; a
  change to what a command prints updates `docs/json.md` and, if an agent would act on it, the
  skill. `src/cli/reference.rs` and `tests/docs.rs` catch the mechanical part.
- **Never prompt without a terminal.** A destructive command asks on a TTY and otherwise requires
  `--yes`.
- **Tests never touch the real machine.** Set `MACRON_CONFIG_DIR`, `MACRON_STATE_DIR` and
  `MACRON_LAUNCH_AGENTS_DIR` to a temp dir, and `MACRON_LAUNCHCTL` and `MACRON_OSASCRIPT` to the
  stand-ins in `tests/support/`, on the child process; `tests/support/mod.rs` does all of it. A
  library test that runs a job passes a `Notifier` for `/usr/bin/true`, so it never posts a real
  notification. `tests/launchd.rs` is the exception: it uses the real launchd, so it is
  `#[ignore]`d. Run it on a Mac before a release with `cargo test --test launchd -- --ignored`.
- **No `std::env::set_var`.** It is `unsafe` in edition 2024 and the crate forbids `unsafe`. Pass
  environment to a child process, or resolve through a lookup closure as `Paths::resolve` does.
- **Never parse `launchctl` output.** Its format is undocumented. Use exit statuses.
- **Only touch plists with macron's label prefix.** Everything else in `~/Library/LaunchAgents`
  belongs to someone else.
- **macOS only.** Do not add `cfg` branches for other platforms.

## Commits

[Conventional Commits](https://www.conventionalcommits.org), because the changelog is generated
from them: `feat: expand step ranges into launchd intervals`, `fix: …`, `docs: …`, `test: …`,
`refactor: …`, `chore: …`, `ci: …`. Lower case after the colon, imperative, no trailing period. A
breaking change to the CLI, the JSON output or `jobs.toml` gets a `!` (`feat!: …`) and a
`BREAKING CHANGE:` footer.

One logical change per commit, each one building and passing tests on its own. Stage files by name;
never `git add -A`.
