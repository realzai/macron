# AGENTS.md

For anyone, human or agent, working **on** macron. An agent that wants to **use** macron to
schedule jobs wants `skills/macron/SKILL.md` instead, once it exists (milestone M4).

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
```

CI runs `fmt --check`, `clippy -D warnings` and `test` on macOS. Run all three before calling a
change done.

## Layout

| Path | What |
|---|---|
| `src/main.rs` | Calls `cli::run`. Nothing else goes here |
| `src/lib.rs` | The library. The CLI and the web UI are both clients of it |
| `src/cli.rs` | Argument parsing and rendering only. No logic a web handler would also need |
| `src/error.rs` | Every error and its stable code |
| `src/paths.rs` | Where config, state and plists live, and the environment overrides |
| `tests/` | The built binary's contract: arguments, output, exit status |
| `docs/plan.md` | The plan |

## Rules

- **One code path for the CLI and the web.** Logic lives in the library. A CLI command and a web
  handler that do the same thing call the same function.
- **Every command supports `--json`.** One JSON document on stdout. Errors on stderr as
  `{"error":{"code":"…","message":"…"}}`. Exit 0 for success, 1 for an error, 2 for a usage error.
- **Error codes are a contract.** Agents match on `Error::code`. Never change what a released code
  means; add a new variant.
- **Never prompt without a terminal.** A destructive command asks on a TTY and otherwise requires
  `--yes`.
- **Tests never touch the real machine.** Set `MACRON_CONFIG_DIR`, `MACRON_STATE_DIR` and
  `MACRON_LAUNCH_AGENTS_DIR` to a temp dir, and `MACRON_LAUNCHCTL` to a fake, on the child process.
  Tests that really load a job into launchd are `#[ignore]`d and run by hand.
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
