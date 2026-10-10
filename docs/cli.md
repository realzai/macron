# Command-line reference

<!-- Generated from src/cli by `MACRON_UPDATE_DOCS=1 cargo test --lib cli_reference`. Do not edit by hand. -->

Every command takes `--json`, which prints JSON instead of text. The JSON each command prints, and every error code, are in [json.md](json.md).

- [`macron ls`](#macron-ls): List jobs with their schedule, next run and state
- [`macron show`](#macron-show): Show one job in full
- [`macron add`](#macron-add): Add a job and install it
- [`macron edit`](#macron-edit): Change a job and reinstall it
- [`macron rm`](#macron-rm): Remove a job and uninstall it
- [`macron enable`](#macron-enable): Install a disabled job again
- [`macron disable`](#macron-disable): Uninstall a job, keeping it in jobs.toml
- [`macron run`](#macron-run): Run a job now, through launchd, as its schedule would
- [`macron runs`](#macron-runs): List recent runs, newest first
- [`macron logs`](#macron-logs): Print a run's output
- [`macron up`](#macron-up): Open the web interface, serving it on 127.0.0.1 until Ctrl-C
- [`macron apply`](#macron-apply): Make launchd match jobs.toml, after editing it by hand
- [`macron doctor`](#macron-doctor): Check for anything that would stop jobs running as configured, and say how to fix it
- [`macron paths`](#macron-paths): Show where macron reads jobs from and writes run history and logs to

## `macron`

```text
Schedule, run and watch local jobs on macOS.

Jobs live in jobs.toml (see `macron paths`). macron installs each enabled job into launchd, which
runs it on schedule, including runs missed while the Mac was asleep.

Usage: macron [OPTIONS] <COMMAND>

Commands:
  ls       List jobs with their schedule, next run and state [alias: list]
  show     Show one job in full
  add      Add a job and install it
  edit     Change a job and reinstall it
  rm       Remove a job and uninstall it
  enable   Install a disabled job again
  disable  Uninstall a job, keeping it in jobs.toml
  run      Run a job now, through launchd, as its schedule would
  runs     List recent runs, newest first
  logs     Print a run's output
  up       Open the web interface, serving it on 127.0.0.1 until Ctrl-C
  apply    Make launchd match jobs.toml, after editing it by hand
  doctor   Check for anything that would stop jobs running as configured, and say how to fix it
  paths    Show where macron reads jobs from and writes run history and logs to
  help     Print this message or the help of the given subcommand(s)

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## `macron ls`

```text
List jobs with their schedule, next run and state

Usage: macron ls [OPTIONS]

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -h, --help
          Print help
```

## `macron show`

```text
Show one job in full

Usage: macron show [OPTIONS] <JOB>

Arguments:
  <JOB>
          The job's name

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -h, --help
          Print help
```

## `macron add`

```text
Add a job and install it

Usage: macron add [OPTIONS] --schedule <SCHEDULE> <JOB> -- <COMMAND>...

Arguments:
  <JOB>
          The job's name: lowercase letters, digits, `-` and `_`

  <COMMAND>...
          The command, after `--`. One argument is taken as a shell command line; several are quoted
          and joined into one

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -s, --schedule <SCHEDULE>
          When to run: cron such as `0 9 * * 1-5`, a shorthand such as `@daily`, or `@every 15m`

  -d, --description <DESCRIPTION>
          What the job is for

      --workdir <DIR>
          The directory to run in, absolute or under `~` [default: ~]

      --shell <PATH>
          The shell to run the command with [default: /bin/zsh]

  -e, --env <KEY=VALUE>
          An environment variable for the command; repeat for more

      --timeout <DURATION>
          Stop a run that takes longer than this, such as `30m` or `1h 30m`

      --notify <NOTIFY>
          When to post a macOS notification about a finished run [default: failure]

          [possible values: never, failure, always]

      --keep-runs <N>
          How many runs to keep, with their output [default: 100]

      --disabled
          Add the job without installing it

  -h, --help
          Print help

Examples:
  macron add backup --schedule '0 9 1 * *' -- ~/bin/backup.sh
  macron add sweep -s '@every 15m' --timeout 5m -- 'cd ~/inbox && ./sweep --all'
  macron add report -s '30 8 * * MON-FRI' -e REPORT_TO=me@example.com -- ~/bin/report
```

## `macron edit`

```text
Change a job and reinstall it

Usage: macron edit [OPTIONS] <JOB> [-- <COMMAND>...]

Arguments:
  <JOB>
          The job's name

  [COMMAND]...
          A new command, after `--`. One argument is taken as a shell command line; several are
          quoted and joined into one

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -s, --schedule <SCHEDULE>
          When to run: cron such as `0 9 * * 1-5`, a shorthand such as `@daily`, or `@every 15m`

  -d, --description <DESCRIPTION>
          What the job is for

      --workdir <DIR>
          The directory to run in, absolute or under `~` [default: ~]

      --shell <PATH>
          The shell to run the command with [default: /bin/zsh]

  -e, --env <KEY=VALUE>
          An environment variable for the command; repeat for more

      --timeout <DURATION>
          Stop a run that takes longer than this, such as `30m` or `1h 30m`

      --notify <NOTIFY>
          When to post a macOS notification about a finished run [default: failure]

          [possible values: never, failure, always]

      --keep-runs <N>
          How many runs to keep, with their output [default: 100]

      --unset <FIELD>
          Set a field back to its default; repeat for more. Applied before the options that set
          fields, so `--unset env --env A=1` replaces the environment

          [possible values: description, workdir, shell, env, timeout, notify, keep-runs]

      --unset-env <KEY>
          Remove one environment variable; repeat for more

  -h, --help
          Print help

Examples:
  macron edit backup --schedule '0 10 1 * *'
  macron edit backup --unset timeout --env MODE=full
  macron edit backup -- ~/bin/backup.sh --verbose
```

## `macron rm`

```text
Remove a job and uninstall it

Usage: macron rm [OPTIONS] <JOB>

Arguments:
  <JOB>
          The job's name

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -y, --yes
          Remove without asking. Required when there is no terminal to ask on

      --purge
          Also delete the job's run history and logs

  -h, --help
          Print help
```

## `macron enable`

```text
Install a disabled job again

Usage: macron enable [OPTIONS] <JOB>

Arguments:
  <JOB>
          The job's name

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -h, --help
          Print help
```

## `macron disable`

```text
Uninstall a job, keeping it in jobs.toml

Usage: macron disable [OPTIONS] <JOB>

Arguments:
  <JOB>
          The job's name

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -h, --help
          Print help
```

## `macron run`

```text
Run a job now, through launchd, as its schedule would

Usage: macron run [OPTIONS] <JOB>

Arguments:
  <JOB>
          The job's name

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -w, --wait
          Print the run's output as it happens, and exit as the run did

  -h, --help
          Print help
```

## `macron runs`

```text
List recent runs, newest first

Usage: macron runs [OPTIONS] [JOB]

Arguments:
  [JOB]
          Only this job's runs

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -n, --limit <LIMIT>
          How many runs to list

          [default: 20]

      --status <STATUS>
          Only runs that ended this way, or are still running

          Possible values:
          - running
          - succeeded: The command exited with status 0
          - failed:    The command exited with another status, was killed by a signal, or could not
            start
          - timed_out: The job's timeout stopped the command
          - lost:      The run never finished recording: the Mac shut down, or `macron exec` was
            killed

  -h, --help
          Print help (see a summary with '-h')

Examples:
  macron runs
  macron runs backup --limit 5
  macron runs --status failed
```

## `macron logs`

```text
Print a run's output

Usage: macron logs [OPTIONS] <JOB>

Arguments:
  <JOB>
          The job's name

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

      --run <ID>
          Which run, by the id `macron runs` shows [default: the latest]

  -f, --follow
          Keep printing output as it arrives, until the run ends

      --raw
          Print only the command's output: no times, no header and no notes from macron

  -h, --help
          Print help

Examples:
  macron logs backup
  macron logs backup --run 42
  macron logs backup --follow
  macron logs backup --raw | grep error

With --json, prints the run and its lines as one document, or with --follow, one JSON object per
line as it arrives and then the finished run as {"run": ...}.
```

## `macron up`

```text
Open the web interface, serving it on 127.0.0.1 until Ctrl-C

Usage: macron up [OPTIONS]

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

      --port <PORT>
          The port to serve on [default: 6220, or any free port if that is taken]

      --no-open
          Print the link instead of opening a browser

      --rotate-token
          Replace the sign-in secret, signing out every browser

  -h, --help
          Print help
```

## `macron apply`

```text
Make launchd match jobs.toml, after editing it by hand

Usage: macron apply [OPTIONS]

Options:
      --dry-run
          Show what would change without changing it

      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -h, --help
          Print help
```

## `macron doctor`

```text
Check for anything that would stop jobs running as configured, and say how to fix it

Usage: macron doctor [OPTIONS]

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -h, --help
          Print help
```

## `macron paths`

```text
Show where macron reads jobs from and writes run history and logs to

Usage: macron paths [OPTIONS]

Options:
      --json
          Print JSON instead of text. Errors go to stderr as JSON too

  -h, --help
          Print help
```
