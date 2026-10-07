# lanes

A runner for parallel, non-interactive agent sessions ("lanes"). It starts Claude Code and omp runs from brief files,
spreads them over several accounts under per-account caps, keeps them under quota floors, resumes the ones that end
early or hit a rate limit, and shows one live board of everything.

- **One command per job.** `lanes start NAME --cwd DIR --prompt BRIEF` launches a detached run and records it;
  `lanes queue` holds it until a slot is free.
- **Accounts and caps.** Each harness is a Claude Code config directory (or omp) with a cap on concurrent lanes. Caps
  per model and a list of banned models are config too.
- **A supervisor** resumes lanes that ended without a report, retries rate-limited ones when the limit lifts, and starts
  queued lanes.
- **Quota floors.** No lane starts or resumes on a Claude account that has used its weekly or 5-hour allowance up to
  your floor, and omp models have their own floors.
- **A board** (`lanes watch`), machine-readable status (`lanes status --json`) and an event stream (`lanes events`).
- **Two optional modules**, off unless configured: a shared database server with a cloned database per lane, and a pool
  of reusable git worktrees.

Linux only: it reads `/proc`, uses `flock`, `nice` and `timeout`.

## Requirements

| Needed for          | Tool                                                   |
| ------------------- | ------------------------------------------------------ |
| everything          | [Deno](https://deno.com) 2 or later                    |
| the board           | [gum](https://github.com/charmbracelet/gum)            |
| Claude Code lanes   | the `claude` CLI, signed in once per account directory |
| omp lanes           | the `omp` CLI                                          |
| the database module | PostgreSQL (`initdb`, `pg_ctl`, `psql`, `pg_isready`)  |
| its Redis part      | Redis (`redis-server`, `redis-cli`)                    |
| the worktree pool   | git                                                    |

`lanes status --json` needs no gum.

## Setup

```sh
cd lanes
cp lanes.config.example.json lanes.config.json   # git-ignored; edit it
CLAUDE_CONFIG_DIR=~/.claude.account1 claude      # sign each account in once
./lanes status                                   # an empty board means it works
```

Put `lanes` on your `PATH`, or call it by path. The config file is `lanes.config.json` next to the `lanes` script, or
the file named by `$LANES_CONFIG`. State (the registry, lane outputs, logs) lives under `root`, which `$LANES_ROOT`
overrides.

A lane's brief is a plain text file. `rules.example.md` is a template of general lane rules to prepend to your briefs.

## A first lane

```sh
./lanes start fix-parser --cwd ~/work/project --prompt ~/briefs/fix-parser.txt \
  --harness claude1 --model sonnet --effort high --budget 20
./lanes watch              # the live board; Ctrl+C leaves it
./lanes status --all       # includes ended lanes
```

The lane's whole output goes to `<root>/fix-parser.1.json`; each resume writes the next attempt number.

## Commands

| Command                            | What it does                                                                              |
| ---------------------------------- | ----------------------------------------------------------------------------------------- |
| `start NAME`                       | Launch a lane now, or refuse with the reason (cap, limit, floor, file-scope overlap).     |
| `queue NAME`                       | Hold a lane until its harness has a slot; `--after LANE` also waits for that lane's DONE. |
| `release NAME`                     | Drop a queued lane's `--after` wait.                                                      |
| `resume NAME`                      | Continue an ended lane in its own session; `--message` replaces the resume prompt.        |
| `stop NAME`                        | Signal the lane's whole process group; a stopped lane is never resumed.                   |
| `forget NAME`                      | Remove an ended lane from the registry (its output files stay).                           |
| `status`, `watch`, `events`        | The board once, the live board, and a change stream.                                      |
| `quota`                            | Each omp floor and each Claude account's session and week, with its verdict.              |
| `supervise`                        | Run the supervisor in the foreground.                                                     |
| `supervisor status\|restart\|stop` | Inspect or control the background supervisor.                                             |
| `db new\|drop\|sweep`              | The database module.                                                                      |
| `worktree take\|fill\|status`      | The worktree pool.                                                                        |

`start` and `queue` options: `--cwd DIR` and `--prompt FILE` (required), `--harness`, `--model`, `--effort`,
`--budget USD`, `--tools SET`, `--readonly`, `--globs A,B`, `--task TEXT`, `--chrome`, `--force`; `queue` adds
`--after LANE` and `--light`. `lanes --help` lists every option with its default.

- `--tools` picks a toolset. Claude lanes: `ro` (read, search), `edit` (plus edit and write) or `full` (plus Bash, the
  default). omp lanes: `ro`, `edit` or `none`. omp lanes never get a shell. `--readonly` means `ro`.
- `--globs` declares the files a lane writes. A new lane whose globs overlap a running writer's is refused, so two lanes
  do not edit the same files.
- `--chrome` starts a Claude lane with the Claude in Chrome extension.
- `--light` exempts a queued lane from the machine gate (see `machine`).
- `--force` overrides the caps, limit, floor and overlap refusals.

## The supervisor

`lanes supervisor restart` starts it in the background (its log is `<root>/supervisor.log`). Every `supervisor.interval`
seconds it:

1. **Re-reads every lane.** A lane is RUNNING while its process is alive (the pid is checked with its start time, so a
   recycled pid is never signalled or trusted). Once it ends, its output decides its state.
2. **Resumes** a lane that ended EARLY_END (a report that says it will wait, or a one-liner) or ERROR, up to
   `supervisor.maxResumes` times, in its own session and on its own account, with `resumePrompt`.
3. **Retries rate limits.** A RATE_LIMIT lane is resumed 2 minutes after the reset time its output states, or 15 minutes
   after it ended when none is stated. Until that moment `start` on that account refuses.
4. **Applies the floors.** An account at its weekly or 5-hour floor (`claudeQuota`) starts and resumes nothing, and says
   why on the board. A queued lane whose account is at a floor moves to the account with the most weekly headroom that
   has a free slot and is under both floors; with none, it keeps its place.
5. **Gates on the machine.** It starts queued Claude lanes and resumes lanes only while the load average is under
   `machine.maxLoad` and available memory is over `machine.minFreeGb`, and at most `machine.startsPerTick` per pass.
   (`lanes start` by hand is not gated by the machine.)
6. **Drains the queue**, oldest first, respecting caps, `--after` and floors. A queued lane that can never start (its
   brief is gone, its model is banned) is dropped with the reason.
7. **Sweeps the database module**, when it is configured.

A lane in BUDGET, NO_OUTPUT, or ERROR/EARLY_END with its resumes spent needs a person: the board lists it under "needs a
human". STOPPED is never resumed. Without a supervisor nothing resumes and the queue does not drain; `events` says so
when it is not running.

Claude usage (`claude -p /usage`, no model call) is read at most once per `claudeQuota.cacheMinutes` per account and
cached in `<root>/claude-usage.json`. An account whose usage cannot be read is not gated: its limit still arrives as
RATE_LIMIT.

## omp lanes

omp is the second harness kind. An omp lane runs `omp -p` in JSON mode with its own session directory under
`<root>/omp-sessions/`, a `--thinking` level taken from `--effort`, and `launch.ompMaxTime` as its time limit. Its state
and cost come from the JSON events it prints, and it resumes by session id like a Claude lane. `lanes quota` reads
`omp usage`; each `quotaFloors` entry names a model pattern, the section of that output to read, and the percentage left
at or below which no lane of that model starts. An unreadable quota refuses too: the runner does not start a
rate-limited model blind. omp has no account to switch, so its harness has a `cap` and, if you need one, an `env` map.

## Status JSON

`lanes status --json` (add `--all` for ended lanes) prints a list, live lanes first, then queued ones. Every row has
exactly these fields:

| Field     | Meaning                                                                                          |
| --------- | ------------------------------------------------------------------------------------------------ |
| `name`    | the lane's name                                                                                  |
| `harness` | the harness key from the config                                                                  |
| `model`   | the model as given to the harness                                                                |
| `effort`  | the effort level, or `null`                                                                      |
| `status`  | RUNNING, RATE_LIMIT, QUEUED, or when `--all`: DONE, EARLY_END, ERROR, BUDGET, NO_OUTPUT, STOPPED |
| `task`    | the `--task` label, or `null`                                                                    |
| `resumes` | how many times it has been resumed                                                               |
| `after`   | the lane a queued lane waits for, or `null`                                                      |
| `waiting` | why a queued lane is still queued, or `null`                                                     |

`lanes events` prints one line per state change, per new commit in a lane's working directory, and a periodic heartbeat.
The registry itself is `<root>/registry.json`: plain JSON, parsed strictly (an unknown key is an error), written
atomically under an `flock` on `<root>/.registry.lock`.

## Config reference

Every key but `harnesses` has a default; a file of one harness is complete. `~` expands to `$HOME`; relative paths
resolve against the config file.

| Key                         | Default                       | Meaning                                                                     |
| --------------------------- | ----------------------------- | --------------------------------------------------------------------------- |
| `harnesses.NAME.kind`       | required                      | `claude` or `omp`                                                           |
| `harnesses.NAME.home`       | required for claude           | the account's `CLAUDE_CONFIG_DIR`                                           |
| `harnesses.NAME.cap`        | required                      | concurrent lanes; with `0` only `--force` starts one, and usage is not read |
| `harnesses.NAME.env`        | none                          | extra environment variables for that harness's lanes                        |
| `defaults.harness`          | first harness                 | used by `start` and `queue` without `--harness`                             |
| `defaults.model`, `.effort` | `sonnet`, `high`              | likewise                                                                    |
| `defaults.budget`           | `50`                          | USD per lane attempt (`--max-budget-usd`); also each automatic resume's     |
| `root`                      | `state`                       | where the registry, outputs and logs live                                   |
| `path`                      | `[]`                          | directories appended to `PATH` for lanes and tools                          |
| `unsetEnv`                  | `[]`                          | variables removed from every lane's and tool's environment                  |
| `laneEnv`                   | `{}`                          | variables added to every lane's environment                                 |
| `marker`                    | first 60 characters           | regex run on the brief; its match finds the lane's transcript for the board |
| `resumePrompt`              | a continue-and-finish message | what an automatic resume says                                               |
| `modelCaps`                 | `{}`                          | `{ "model": n }`: at most n running lanes on that model                     |
| `banned`                    | `{}`                          | `{ "model regex": "reason" }`: starting such a model is refused             |
| `quotaFloors`               | `[]`                          | omp floors: `{ "model": regex, "section": "...", "floor": percent left }`   |
| `claudeQuota.weeklyMax`     | `90`                          | percent of the week used at which an account stops taking lanes             |
| `claudeQuota.sessionMax`    | `90`                          | the same for the 5-hour session                                             |
| `claudeQuota.cacheMinutes`  | `10`                          | how long a usage read is reused                                             |
| `machine.maxLoad`           | `1000` (off)                  | no Claude lane starts above this 1-minute load average                      |
| `machine.minFreeGb`         | `0` (off)                     | nor below this much available memory                                        |
| `machine.startsPerTick`     | `2`                           | Claude starts and resumes per supervisor pass                               |
| `supervisor.interval`       | `120`                         | seconds between passes                                                      |
| `supervisor.maxResumes`     | `3`                           | automatic resumes per lane                                                  |
| `launch.nice`               | `10`                          | `nice` level of every lane                                                  |
| `launch.timeoutSeconds`     | `14400`                       | wall-clock limit of one attempt, via `timeout`                              |
| `launch.ompMaxTime`         | `45m`                         | omp's own time limit                                                        |
| `db`                        | module off                    | see below                                                                   |
| `worktrees`                 | module off                    | see below                                                                   |

## The database module

Add a `db` key to give lanes isolated databases without a server each. One PostgreSQL server (and optionally one Redis
server) runs under `<root>/<dir>` with fsync off, for data nobody keeps. A lane asks for a database; it is cloned from a
template that is built once per template identity, in about the time a `CREATE DATABASE ... TEMPLATE` takes.

```sh
eval "$(lanes db new my-lane --worktree /path/to/lane/worktree)"
# exports the variables from db.env; run your suites against them
lanes db drop my-lane      # or let the supervisor drop it once the lane is DONE or STOPPED
```

| Key                           | Default            | Meaning                                                                    |
| ----------------------------- | ------------------ | -------------------------------------------------------------------------- |
| `db.dir`                      | `db`               | server directory, under `root`                                             |
| `db.pgPort`                   | `54330`            | PostgreSQL port (listens on 127.0.0.1)                                     |
| `db.maxConnections`           | `200`              | cover every lane's connections at once                                     |
| `db.sharedBuffers`            | `256MB`            |                                                                            |
| `db.initdbArgs`               | `["-E", "UTF8"]`   | extra `initdb` arguments, such as a locale                                 |
| `db.templateInputs`           | `[]`               | paths in the worktree (files or directories) that decide the template      |
| `db.migrateCommand`           | none               | argv run in the worktree to build a template; none gives an empty database |
| `db.env`                      | see below          | variables printed by `db new`, with `{pg}`, `{database}`, `{redis}`        |
| `db.redis.port`, `.databases` | off; `63790`, `16` | set `db.redis` to enable Redis; each lane holds one numbered database      |
| `db.templateKeepMinutes`      | `120`              | how long an unused template is kept                                        |
| `db.orphanHours`              | `24`               | age at which a database no registry lane owns is dropped                   |

The template is keyed on a hash of everything under `db.templateInputs`: two worktrees whose inputs are identical share
a template, and the first lane on a new hash builds it while the others wait. `db.migrateCommand` runs in the worktree
against an empty database it created, with the `db.env` variables set to point at it. The default `db.env` is
`DATABASE_URL` (`{pg}/{database}`), `DATABASE_ADMIN_URL` (`{pg}/postgres`, for suites that create scratch databases)
and, with Redis, `REDIS_URL` (`{redis}`). Asking again from the same worktree contents returns the same database; after
the inputs change you get a fresh one. The supervisor drops databases of DONE and STOPPED lanes, stale orphans and
unused templates, and restarts a server that died.

## The worktree pool

Add a `worktrees` key to recycle installed git worktrees instead of paying for a fresh checkout and dependency install
per lane. A worktree you no longer need is moved into the pool detached; `take` hands it out again on a new branch,
cleans untracked files and keeps ignored ones (installed dependencies stay).

```sh
lanes worktree take /path/to/new --branch lane/fix-parser   # --from REF; default origin/main
lanes worktree fill      # pool every recyclable worktree under the pool directory
lanes worktree status
```

| Key                           | Default          | Meaning                                                                  |
| ----------------------------- | ---------------- | ------------------------------------------------------------------------ |
| `worktrees.repo`              | `.`              | the repository                                                           |
| `worktrees.dir`               | `.worktrees`     | where worktrees live, relative to the repository                         |
| `worktrees.remote`, `.branch` | `origin`, `main` | the base: a worktree is recyclable once its HEAD is in `remote/branch`   |
| `worktrees.installCommand`    | none             | argv run in a taken worktree (for example a lockfile-frozen install)     |
| `worktrees.installedMarker`   | none             | a path that must exist for a worktree to be pooled (e.g. `node_modules`) |
| `worktrees.keep`              | `[]`             | regexes of worktree names that are never pooled                          |

A worktree is recyclable when it is clean, installed, already contained in the base branch, not named in the brief of a
live or queued lane, and not matching `keep`. With an empty pool `take` falls back to `git worktree add`.

## Safety notes

- **Lanes act as you.** A `full` Claude lane runs Bash with your permissions and `--permission-mode acceptEdits`; the
  runner does not sandbox it. Give lanes `ro` or `edit` when a shell is not needed, set `--budget`, and keep
  `launch.timeoutSeconds` finite.
- **Account directories hold credentials.** Keep `home` directories and `lanes.config.json` out of version control; the
  example config is the only config file that is tracked.
- **Never kill by pattern.** `stop` signals the lane's own process group by the pid the registry recorded, after
  checking the pid's start time. Do the same by hand; `rules.example.md` makes it a lane rule.
- **Account addresses are scrubbed** from `omp usage` output before it reaches the board or a log.
- **One supervisor.** A second `supervise` refuses to start; `supervisor restart` signals the old one under the registry
  lock, so it is never stopped between launching a lane and recording it.
- **The registry is the source of truth.** It is saved even when a command fails, so a launched lane is never
  unrecorded; a hand edit that does not parse is shown by the board and retried, not crashed on.

## Development

```sh
deno task ok     # fmt --check, lint, type-check, and the tests
```

Tests build their subjects through the same wiring as the CLI. The launcher, the usage readers and the database servers
are seams, replaced by fakes in the tests; the worktree tests drive real git in temporary repositories, and a few tests
start real child processes (`sleep`, `flock`, `deno`). They read `src/test.config.json`, never your config.

## License

MIT, see [LICENSE](LICENSE).
