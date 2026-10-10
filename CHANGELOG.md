# Changelog

What changed in each release of macron. Versions follow [Semantic Versioning](https://semver.org).

## [0.1.0](https://github.com/realzai/macron/releases/tag/v0.1.0) - 2026-10-11

The first release.

### Added

- Jobs kept in `~/.config/macron/jobs.toml` and installed into launchd: `macron add`, `edit`,
  `rm`, `ls`, `show`, `enable` and `disable`, and `macron apply` after editing the file by hand.
  Changes keep the file's comments.
- Schedules in five-field cron, the shorthands such as `@daily`, and `@every <duration>`, each
  described in words with its next runs.
- A record of every run and its output: `macron run`, `runs` and `logs`, with `--follow`, timeouts,
  and a notification when a run fails.
- A web interface, `macron up`, that does everything the command line does and shows a running
  job's output as it arrives. It is served on 127.0.0.1 to the browser it signs in, and to nothing
  else.
- `macron doctor`, which checks what can stop a job running and says how to fix it.
- `--json` on every command, described in [`docs/json.md`](docs/json.md), and a skill that
  teaches agents to use macron: `npx skills add realzai/macron`.
