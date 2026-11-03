# relay

Queue coding-agent tasks for any git repository and let them run unattended:
each task is a headless session of an agent harness (Claude Code or omp)
inside a bubblewrap sandbox, on whichever account of whichever
harness has the most quota to spare. When an account runs out mid-task, the
session moves on: to another account of the same harness with its transcript,
or afresh to another harness.

`relay ask` is the other half: a one-shot, read-only call to any harness, now,
in your own account ([the skill](../.agents/skills/relay/SKILL.md)).

```sh
relay queue add fix-parser --prompt briefs/fix-parser.md --model claude=sonnet
relay view status                 # who is working on what, and each account's room
relay view result fix-parser      # the session's final report, branch and cost (--wait: once it ends)
relay land fetch fix-parser       # its branch, copied into your repository
```

Linux only. It replaces `lanes`, the Deno runner that was here before. See
[Coming from lanes](#coming-from-lanes).

## Requirements

- Linux, [bubblewrap](https://github.com/containers/bubblewrap) (`bwrap`), git, `gzip`, and `nice` and
  `timeout` from coreutils.
- At least one harness, signed in once per account: `claude` per
  `CLAUDE_CONFIG_DIR=~/.claude.work claude`, `omp` per profile
  (`~/.omp/profiles/<name>`).
- Rust 1.99 to build it (the toolchain file pins it).

```sh
cargo install --path relay --locked      # puts `relay` in ~/.cargo/bin
cp relay/relay.example.toml ~/work/myproject/relay.toml   # then edit `name`
cd ~/work/myproject && relay probe       # checks the sandbox; starts no model session
```

relay finds its config from `--config`, then `$RELAY_CONFIG`, then the nearest
`relay.toml` at or above the current directory. [`relay.example.toml`](relay.example.toml)
lists every key with its default. Unknown keys are errors. The file holds
per-machine choices (accounts, limits), so a project shared across machines
usually gitignores it.

## What a job is

A job is a name plus a **brief**, the only context the session gets. It is
passed on stdin or as a private file, never in argv, so it can be any size and is not visible in
`/proc`.

- **Build jobs** run in a private clone (`git clone --shared`, which is small
  and fast) on the branch `relay/<name>`. The session commits there. Pushing is
  disabled, and your repository is mounted read-only.
- **Read-only jobs** (`--readonly`) run in your repository mounted read-only,
  for reviews and analysis.

**Dependencies.** A job queued `--after` other jobs gets no seat until each is
`done`, so build → review → fix chains need no watching. `view status` shows
`after a, b` (then `after b` once a is done); `--json` carries `after` and
`pending`, the dependencies still to finish. A job counts as done when its
session ended `done`, landed or not: landing stays your step. A build job
waiting for build jobs starts from their branch (below). A dependency that fails,
is stopped, dequeued or cleaned away before it was done never finishes, so the
dependent shows `waits for a (failed)` and does not start on its own, nor keep
the dispatcher awake. Queue the dependency again (or `queue resume` it), or
`relay queue release NAME [--from a]`. Only existing jobs can be named, so there
are no cycles.

**Where a build job starts.** A build job starts from the repository's `HEAD`,
unless it waits for build jobs (`--after`) or forks one (`--fork`): then its
clone starts from that job's branch tip, so it holds the work it depends on
before anyone lands it. With several such jobs, it starts from the tip that
holds all the others; when their branches diverge, so no tip holds them all,
the job fails at once and says so: land one into another, or queue it after
one of them. Read-only jobs read your repository, as before.

**Forks.** `queue add NAME --fork JOB` starts the new job as a fork of JOB's
session (claude: `--resume ID --fork-session`): it begins with JOB's whole
conversation, under a session id of its own, on a harness whose adapter says it
can fork (claude can; omp cannot, and is refused). JOB must have run. Queued
`--after JOB` too, it starts as soon as JOB is done, on JOB's account when that
has room, while the provider's prompt cache still holds the transcript. That is
the only saving: past the cache's lifetime (minutes), a fork reads the whole
transcript again at full input price, so fork only for closely related work
that needs that context, such as a fix to what JOB just built. **A review must
never fork its builder**: it would inherit the builder's reasoning and blind
spots, and independence is what a review is for. Queue reviews fresh.

A read-only job cannot change the repository, so the same brief, for the same
harnesses, models and effort, at the same clean `HEAD` commit, asks the same
question. `queue add` refuses it (exit 4, conflict) while another job is
answering it, or once one has answered it, naming that job (`relay view result
NAME`); `--again` asks anyway. A dirty working tree is never matched, and an
answer counts only if its session read that commit: a job that waited for
quota while the tree moved on answered a different question.

The session is told to end with a short `FINAL` section: a `status` line
(done, partial or blocked), then `commits`, `checked` (each command and pass
or fail, never its output), `decided`, `found` (file:line and the problem) and
a few `notes`, about 40 lines at most. It is told to read only what the task
needs, starting from the files the brief names.

A build job that ends without `FINAL` is asked once more, since ending the
turn ends the job and it may have unfinished commits. A read-only job is not:
its answer, with or without the section, is the result.
[`rules.example.md`](rules.example.md) is a template of general rules for
unattended sessions, each with its reason. Edit it for your project and
prepend it to your briefs, or have each brief point to it.

| Command | |
| --- | --- |
| `relay queue add NAME (--prompt FILE \| --text TEXT) [--readonly] [--harness H,...] [--model H=M] [--effort E] [--budget USD] [--account H:L] [--task TITLE] [--again]` | Queue a job. `--harness` limits where it may run; `--account` pins it, and it waits for that account |
| `relay queue add NAME ... --profile P [--no-readonly]` | Fill what the line leaves unset from `[profiles.P]`; explicit flags win. An unknown profile is refused (exit 3, listing the names) |
| `relay profiles` / `relay config show` | The resolved profiles with their notes / the merged policy and project config, and which file each part came from |
| `relay queue resume NAME --message TEXT [--budget USD] [--model H=M]` | Continue an ended or checking session, on any account with room. `--budget` caps that run alone, not the job's total; `--model` moves the job to another model from this run on |
| `relay queue add NAME ... --after A[,B]` | Start only once every named job is `done` (repeat or comma-separate; unknown names are refused, exit 3) |
| `relay queue add NAME ... --fork JOB` | Start as a fork of JOB's session, from its branch when it is a build job ([forks](#what-a-job-is)) |
| `relay decide list [--wait [--timeout S]]` / `relay decide answer ID --action A ...` | Decisions waiting for the orchestrator / answer one ([hooks](#hooks)) |
| `relay queue release NAME [--from A,...]` | Stop a queued job waiting for some or all of its `--after` jobs |
| `relay queue dequeue NAME` / `relay queue stop NAME` | Withdraw a queued job / end a running one |
| `relay view status [--all]` | Jobs and accounts. With `--json`, the whole folded state |
| `relay view result NAME [--wait]` | Final report, status, cost, branch, log path; `--wait` first blocks until the job ends |
| `relay view watch [--all]` | The status live in a terminal UI (ratatui): jobs, the selected job's latest output and answer, and accounts, redrawn as events land in any project. Click a job to select it, scroll a pane with the wheel, click a key hint; or ↑↓, PgUp/PgDn, `a` older jobs, `q` quit. A reinstalled relay takes the window over by itself (hold Shift to select text) |
| `relay view events` | The merged event log |
| `relay quota show` / `refresh` / `enable L` / `disable L` | Account room for the configured model, with a `for <model>` sub-row for each other model a queued or running job asks of it; a fresh reading; switches |
| `relay land fetch NAME` / `land clean NAME` | Copy the job's branch into your repository (no checkout) / delete its clone and records |
| `relay land gc [--dry-run]` | Remove what finished jobs no longer need: build outputs, old clones, old logs ([disk](#disk)) |
| `relay probe [--account H:L]` | Sandbox self-test for one account of each harness, with controls |
| `relay ask AGENT MODE ...` | One-shot read-only call, outside the queue ([the skill](../.agents/skills/relay/SKILL.md)) |

Every command takes `--json`: one JSON value on stdout, and a failure as
`{"error", "kind"}` on stderr. The exit code says which kind it was, with or
without `--json`: 2 usage, 3 not found, 4 the job is in the wrong state, 5
refused by a rule (a banned model, a disabled harness, the `ask` contract), 1
anything else. An agent driving relay can branch on those, never on wording.

Nothing needs starting by hand. Every command that changes something wakes the
dispatcher, which exits on its own a minute after the queue empties.

## Policy and profiles

The owner's routing rules live in a policy file, not in an orchestrator's memory:
the orchestrator judges what kind of task a job is, the config maps that kind to
harness, model, effort, budget and read-only, and relay enforces what is
mechanical. The file is `$RELAY_POLICY`, else `~/.config/relay/policy.toml`, and
is optional (a `$RELAY_POLICY` that names no file is an error). It is read first,
and the project's `relay.toml` overrides it:

- a project's `[profiles.X]` replaces only the fields it sets;
- a project's `session.banned_models` adds to the policy's and cannot lift a ban.

```toml
[session]
banned_models = { "^minimax-code/" = "MiniMax is banned (owner, 2026-10-21)", "^google-antigravity/" = "Google Antigravity is banned (owner, 2026-10-19)", "^(haiku|sonnet|opus)$" = "use the full id, e.g. claude-opus-5-5" }

[profiles.mechanical]
harness = "claude"
model = "claude-haiku-5-5"
effort = "xhigh"
budget = 3
note = "a precise brief: docs, fixtures, citations, regenerated counts"

[profiles.bug-hunt]
harness = "omp"
model = "openai-codex/gpt-6.1-sol"
effort = "high"
readonly = true
max_running = 1
max_per_day = 3
note = "second-family bug hunt or review of critical code and plans only; costly"
```

A profile has `harness`, `model`, `effort`, `budget`, `readonly`, `note`,
`max_running` and `max_per_day`, all optional. `relay queue add NAME --profile P`
fills each field the command line does not set (`--no-readonly` turns a profile's
read-only off); explicit flags win. The model reaches the same banned-model check
as one given by hand, at queue and at start. The job records its profile, and it
stays with the job across `queue resume`.

Two limits count per profile. `max_running` is the most of its jobs with a
runner at once. `max_per_day` is the most of its sessions to start in a rolling
24 hours, counted from `dispatched` events on the bus, so cleaning a job away
gives nothing back (a retry on another account is a start too). A job they hold
stays queued with the reason on the board and in the dispatcher's log:
`profile bug-hunt: 3 started in 24h (max 3), next slot 14:02Z`.

`relay profiles [--json]` prints each resolved profile with its note, which is
how a fresh orchestrator learns the policy. `relay config show` prints the merged
bans and profiles with `[policy]` or `[project]` after each setting.
`relay view status` and `view watch` show a job's profile where it has one, else
the model it names; `--json` carries both (`profile`, `models`).

## Hooks

relay meets a few moments where a judgment beats a fixed rule. At each
**decision point** it asks a chain of **handlers**, in order, and carries out
only an action from that point's closed set, after checking it against policy:

| point | when | actions |
| --- | --- | --- |
| `cap` | a build job's run hits its spend cap | `continue {budget}`, `redirect {message, budget}`, `stop {why}` |
| `ended` | a run ends done or failed (an error, silence, no FINAL after the follow-up) | `accept`, `resume {message, budget}`, `requeue {profile}`, `follow-up {brief, profile, after, fork, name}`, `stop {why}` |
| `held` | a queued job has been held by a limit for `held_after_minutes` | `wait`, `reroute {harness, model, profile}`, `force` |
| `disk` | a running job's clone is over `limits.job_disk_gb`, or the disk is nearly full ([disk](#disk)) | `wait`, `stop {why}` |

```toml
[hooks]
cap   = ["rule", "agent:checker", "orchestrator"]
ended = ["rule", "orchestrator", "agent:triage"]
held  = ["rule"]
disk  = ["orchestrator"]
held_after_minutes = 60
orchestrator_timeout_minutes = 20   # then the next handler
command_timeout_seconds = 120
allow_force = false                 # whether `held` may answer `force`
```

Every handler may also answer `defer`, which passes the decision to the next.
When all defer, relay does what it does without hooks: at `cap` the job waits
for a human (`failed`, resume with `--budget`), at `ended` the run's outcome
stands, at `held` the job waits, at `disk` the job is stopped.

- `rule`: relay's own checks, free and deterministic. At `cap`, no new commit
  and no changed file since the last checkpoint is `stop`. At `ended`, a done
  run whose FINAL says `status: done` is `accept`, and a run that went silent
  is resumed once in the same session. Otherwise it defers.
- `agent:<profile>`: a short read-only session on that profile's harness, model
  and budget, on another account than the worker's where one is free. Its prompt
  holds the point, the brief, the branch since the last checkpoint, the worker's
  last message, the job's earlier decisions and the JSON shape of each allowed
  action. `limits.checker` with `budget_growth` above 1 is shorthand for
  `cap = ["rule", "agent:checker"]` (see [checkpoints](#checkpoints)).
- `command:<argv>`: runs the program (argv split on spaces) on the host with the
  request as JSON on stdin, and reads one JSON object from stdout:
  `{"action": "continue", "budget": 20, "why": "..."}` (`cost_usd` optional).
  A malformed answer, a failure or a timeout is a `defer`.
- `orchestrator`: parks the request on the bus. `relay decide list --wait`
  blocks on the doorbell until a request is waiting and prints it (`--json`:
  each with its full request); `relay decide answer ID --action A [--message M]
  [--budget USD] [--profile P] [--brief TEXT] [--why W] ...` answers it. Past
  `orchestrator_timeout_minutes` the chain moves on. A runner waiting for an
  answer shows `checking` and holds no seat. `relay view status` lists waiting
  decisions in their own section, so a human sees them too.

**Safety.** relay checks every action before acting: that the point allows it;
that a budget is positive and keeps the job under `limits.job_ceiling_usd`;
that a profile exists, its model is not banned, and its `max_running` and
`max_per_day` have room; that a rerouted model is not banned; that `force` is
allowed; that a fork's harness can fork. A handler's action that fails is
recorded with the refusal and counts as `defer`; `decide answer` refuses it at
once (exit 5). Hooks never land, push or delete: those stay with you.

Every decision is on the bus: `decision-requested {id, job, point}`, `parked
{id, until}` while it waits for the orchestrator, and one `decided {id,
handler, action, why, cost_usd, refused}` per handler asked. A handler's spend
counts toward its job's total. `relay view status --json` has every decision
under `decisions`. A `follow-up` is queued like `queue add` (by default
`--after` the job, named `<job>-next`), `requeue` starts the job afresh under
the profile with its branch kept, and `reroute` and `force` are queue events.

## Disk

A build job's clone can grow without bound (one Rust job's `target/` reached 92G
across feature builds, release, nextest and clippy), and finished jobs' clones
sit until someone cleans them. relay keeps the disk from filling on its own.
**All of this is on with its defaults, with no `[gc]` section at all**, because
it is a safety feature; the keys only tune it.

```toml
[gc]
build_outputs = ["target", "node_modules", ".venv"]  # relative to a clone
outputs_after_hours = 6      # an ended job's build outputs go after this
clones_after_days = 7        # a done job's whole clone goes after this
waiting_retain_days = 7      # a failed or stopped job's clone is kept this long
logs_gzip_after_days = 3     # <job>.log.ndjson is gzipped after this
interval_minutes = 60        # the dispatcher runs a pass this often while awake
pass_seconds = 300           # a pass stops here; the next goes on

[limits]
job_disk_gb = 40             # a running clone over this reaches the `disk` point (0: off)
min_free_disk_gb = 10        # also: a pass runs at once below this, and holds new starts
disk_sample_minutes = 3      # how often a running clone is measured

[session.env]
CARGO_INCREMENTAL = "0"      # suggested for Rust: incremental dirs are much of target/
```

**`relay land gc [--dry-run] [--json]`** removes by rule, never by glob, one
line each (`gc: removed <path> (<size>)`, `gc: would remove ...` in a dry run),
a totals line, and a `collected` event per clone removed or emptied:

- **Build outputs** of a job that is not live (not running, checking or queued)
  and that no queued job waits for (`--after`) or forks, once it has ended
  `outputs_after_hours` ago. The source and the branch stay, so `queue resume`
  works and only rebuilds.
- **Whole clones** of `done` jobs after `clones_after_days`, landed or not; of
  failed or stopped jobs after `waiting_retain_days`, since those may be resumed.
  Every commit of the job's branch must be in the repository first (`land fetch`
  ran, or the tip is reachable from a repository ref); otherwise gc fetches it
  into `refs/relay/rescue/<job>` and says so. A clone with uncommitted work
  (build outputs aside) is kept, and the line says why. A job whose clone is gone
  is resumed from that rescue ref (else its fetched branch).
- **Logs**: `<job>.log.ndjson` of a job that is not live, older than
  `logs_gzip_after_days`, becomes `.log.ndjson.gz` (a later run's output is
  appended as another gzip member). `view result`, `view watch` and every
  reader take either; `view result` falls back to the answer in the log when
  `result.md` is gone, and shows the `.gz` path.
- Never a clone with a live runner, never a symlink (skipped whole), never a
  path outside relay's own clone and state directories. A `build_outputs` entry
  must be a plain relative path (no `..`, not absolute).

The dispatcher runs a pass at most every `interval_minutes` while it is awake
(a stamp in the state directory keeps that across restarts), and at once when
free disk falls under `min_free_disk_gb` (not again within five minutes). A pass
stops cleanly at `pass_seconds` and the next goes on; only one runs at a time.

**A ceiling for running jobs.** The runner measures a build job's clone when it
starts and every `disk_sample_minutes` (a recursive stat walk, bounded in time,
not following links). The size is on the board (`12G`) and in `--json`
(`disk_bytes`). When a clone passes `job_disk_gb`, or free disk falls under
`min_free_disk_gb` while it is the largest clone running, the `disk` [hook](#hooks)
point is reached. With no `disk` chain, or every handler deferring, the job is
stopped with the reason `disk: clone 52G over 40G` or `disk: 8G free, under 20G`,
its clone and session kept: free some space (`land gc` first), then `queue
resume`. `wait` lets it go on, and asks again only after an hour. Nothing is
ever deleted inside a running job's clone.

`[session] env` sets variables in every job's environment, over the caller's and
the development environment's and under the harness's own account variables.
relay hard-codes none: `CARGO_INCREMENTAL = "0"` is the suggestion above.

## Harnesses and accounts

Every harness is scheduled alike. An adapter in [`src/harness/`](src/harness/)
says, for its CLI:

| | claude | omp |
| --- | --- | --- |
| an account | a `~/.claude*` dir with a refreshable sign-in | `~/.omp` (`default`) or `~/.omp/profiles/<name>` |
| selected by | `CLAUDE_CONFIG_DIR` | `OMP_PROFILE` |
| quota from | `claude -p /usage`, and each session's rate events | `omp usage`, per provider |
| resume on another account | yes: the transcript is copied | yes: the session lives with the job, not the profile |

An account is named `harness:label` (`claude:work`, `omp:default`).
`[harness.<id>]` in the config sets a harness's `model`, `effort`,
`extra_args` and `program`, or turns it off; `accounts.exclude` leaves out
single accounts.

Who a claude directory is signed in as comes from its `.claude.json`
(`oauthAccount`), read locally, never by a call. relay shows only the plan and
the first 6 hex characters of a hash of the account id (`Ultra #76de9d`), and
stores and prints no email, name or raw id. A directory with no account is `not
signed in` and is never chosen. Two directories signed in as one account are
shown as `same sign-in as claude:first`; they share one quota (one reading,
the newest) and one seat budget, so a session on either counts against both.
omp keeps no identity relay can read, so it shows none.

Quota readings cost no model call. One is taken again after 10 minutes, or
once a reset it names has passed; a reading that fails is not retried for the
same 10 minutes. An account is used only when:

- it has a reading, and not a stale one (unread or stale is not the same as
  having room; a job waits while a fresh one is taken);
- the meters that govern the job's model are below `limits.floor` (omp meters
  each provider, so its model must name one: `minimax-code/MiniMax-M3.1-Flash`);
- it is not limited or signed out;
- it has a free seat.

Of those, relay picks the one with the most room left per day until reset,
because quota still unused at a reset is lost. A session that already ran
stays on its account while that account is usable, since moving it loses the
provider's prompt cache and its whole transcript is read again at full price.

Accounts belong to the machine, not the project, so every project on it
shares what it knows of them: each listens to the others' state (never
writing it) and uses the latest reading, the later limit, and the latest
sign-in change (a sign-out stands until the account is enabled again in any
project), so a reading one project took is not taken again. Seats are shared
too: `limits.per_account` counts the sessions every project runs on the
account, and one machine-wide lock keeps two dispatchers from giving out the
same seat. Each dispatcher's doorbell rings for every project's events.
Whether an account is used at all (`relay quota enable` and `disable`) and
`limits.max_running` stay per project.

When a session hits a limit, its account is closed until two minutes after the
stated reset (or for 15 minutes if none is stated), and the job goes back in
the queue. A session whose sign-in has expired, or a `/usage` reading whose output
says the session is expired, unauthorised or not signed in, marks its account
signed out (`signed out since <time>` on the board; no extra call is made, and
it is asked again only when a reading would be stale). It is used again once a
later reading succeeds, when its sign-in changes on disk (a new login), or
with `relay quota enable H:L`. A session that
dies with no output is retried `limits.max_retries` times. A session that
reaches its spend cap is not resumed automatically unless you opt in to
checkpoints (below), because spending more is your call. Budget, wall clock and
silence limits are enforced by relay, the same for every harness.

### Checkpoints

A spend cap can be a checkpoint instead of a stop. With `limits.budget_growth`
above 1, a build job that reaches its cap goes to `checking`, and relay runs a
short read-only progress check on `limits.checker`, on another account than
the worker's where one is free:

```toml
[limits]
budget_growth = 2.0      # the next run's cap is the last one times this
job_ceiling_usd = 120.0  # all runs and checks of one job; absent: unbounded
checker = { harness = "claude", model = "sonnet", budget_usd = 1.0 }
```

The check reads the brief, every earlier verdict, `git log --stat` since the
last checkpoint (with the diff when it is small) and the worker's last message,
and answers `continue`, `redirect` (with a correction the worker reads next) or
`stop`. On `continue` or `redirect` the same session resumes with the cap
times the growth; on `stop` the job is left `failed` for you. No new commit and
no changed file since the last checkpoint is a `stop` that asks no model, and a
next cap that would take the job past `job_ceiling_usd` always waits for you.
Read-only jobs, and any job without growth, stop at their cap as before.
`relay view status` and `view result` show the latest checkpoint
(`checkpoint 2: continue → $30.00`); `--json` has all of them under
`checkpoints`. `relay queue resume NAME --budget X --message M` works from
`checking` or `failed` as ever. This is the `cap` [hook](#hooks) with its
default chain, `["rule", "agent:checker"]`; `hooks.cap` replaces the chain, and
with it set a cap is a checkpoint even without growth (the next cap is the
action's `budget`, else the same cap again).

## The sandbox

Each session runs under `bwrap`. It sees the machine read-only, except for:

- **Hidden** (shown empty): `sandbox.hide`, by default your repository's
  siblings; every account of every harness but its own; relay's own state and
  other jobs' clones; the credential stores in `sandbox.hide_home`; and host
  sockets (`/run/user/<uid>`, sockets under `/run`), since a read-only mount
  does not stop a connection to a socket.
- **Writable**: its own clone and job directory, and its own account, except
  what in it the host later runs or obeys (claude's settings, hooks, plugins,
  skills and scripts; omp's config, agents, extensions and natives), which is
  mounted back read-only.
- **Caches** (`sandbox.caches`, default `~/.cache` and cargo's registry and
  git checkouts): writable, but relay's own directory for the project, shared
  by its sessions and never the host's. A session that could write the host's
  `~/.cargo` could replace what the host runs. The first build fetches afresh.

What is left open, by design or by necessity:

- **The network**, since the session needs its provider's API, and with it
  abstract Unix sockets.
- **claude's `.claude.json`**, which can name MCP servers the host would start
  later. claude rewrites it by renaming a new file over it, which a read-only
  mount would break.

The mounts are one declarative plan in layers ([`src/sandbox.rs`](src/sandbox.rs)),
where the job's own mounts come last, so nothing above can hide them.

Environment variables are cleared and passed back in, except those matching
`sandbox.env_deny` (tokens, secrets, keys, `AUTH`, cookies, sessions and
`CLAUDE*`). The adapter then sets the one that selects the account. A
project whose toolchain comes from a development environment that cannot
start inside the sandbox (single-user Nix locks its store, which is
read-only there) names it in `session.env_shell`, such as
`["nix", "develop", "-c"]`: relay enters it on the host before each attempt,
as direnv would, passes its variables in through the same filter, and tells
the session its tools are already loaded. Signing
keys are hidden, so git inside is told not to sign (`commit.gpgsign` and
`tag.gpgsign` off); sign commits when you land them.

The session also gets its own PID namespace, so nothing it starts outlives it.
With the sandbox as the boundary, the session runs without approval prompts. Without it
(`sandbox.enabled = false`), read-only jobs use the harness's read-only mode
and build jobs its edits-only mode.

`relay probe` checks the plan from the inside, for one account of each
harness: every hidden path is empty and no socket, with a control proving it
exists outside; every writable one is writable and every read-only one is not;
and every cache shows relay's directory, not the host's.

relay's state directory is owner-only, since it holds briefs, answers and
resume messages.

Sessions also run with:

- **No account extras:** no MCP servers, no user settings or plugins, and no
  skill listings. Each costs context on every turn, and a queued task does not
  use them.
- **Limits:** `nice` for CPU priority, a wall-clock limit (`session.max_hours`)
  and a silence limit (`limits.silent_minutes`).

## How it is built

relay follows a systems model. Each system has one responsibility, runs as its
own process, and owns one output port:

- **The port:** an append-only file, `bus/<system>.ndjson`, holding only that
  system's event types. A port is built only from its system's `Source`, a
  capability only that system's module can make, so a second writer cannot
  be built. A line that is not its file's type is dropped on read.
- **One writer per attempt:** each attempt at a job is a session system of its
  own, writing `bus/session/<job>.<created>.<attempt>.ndjson`. The fold hears
  a session event only from the job's current attempt of its current
  incarnation (a name cleaned and queued again is a new one), about that job,
  so a runner that outlived its attempt writes where nobody listens. The files
  are kept: they also hold account facts (limits, sign-outs) the record needs.
- **State:** everything is derived by folding the bus. The fold is kept as a
  checkpoint (`world.json`, with how far each file was read), so a command
  reads only what is new; any doubt (a new version, a replaced file, an event
  stamped earlier than one already folded) means a full fold. The checkpoint
  is a cache, never the record.
- **Wake-ups:** the files are the lossless record, and an inotify watch is
  only a doorbell. A runner's exit rings it too. Otherwise the dispatcher
  sleeps until the next moment the plan could change by itself (a limit
  lifting, a meter resetting, a reading going stale), with a 5-minute
  backstop, and exits after a minute with nothing to do.
- **Single roles:** the dispatcher and the quota reader each hold a `flock`,
  which the kernel releases if the holder dies.

| System | Responsibility | Emits |
| --- | --- | --- |
| queue | takes work in (under a profile, if asked), withdraws it, stops it, releases it from its `--after` jobs, reroutes or forces a held one | queued, released, resumed, rerouted, forced, dequeued, stop-requested |
| dispatch | gives queued jobs a seat on an account; reaps dead runners | dispatched, reaped |
| session | runs one job, carries out the decisions at its cap and its end, measures its clone and stops it at the disk ceiling, and reports what happened | cloned, started, rate, limited, signed-out, follow-up, requeued, checking, checked, disk, ended |
| decide | runs a decision point's handler chain, in the process that reached it (a runner, or `decide held`, which dispatch starts); takes the orchestrator's answers | decision-requested, parked, decided |
| quota | account readings and switches | reading, unreadable, signed-out, account-enabled |
| land | brings branches back; cleans up, and gc | forgotten, collected |
| view, probe | read-only | — |

A runner is identified by its pid together with its start time, so a reused
pid is never mistaken for it or signalled. It starts behind a gate (a pipe on
its stdin) that opens only once its dispatch is recorded, so it never reads the
bus before its own account is on it.

## Coming from lanes

relay keeps lanes' hard-won rules:

- **Prompts:** sent on stdin, and a session id is chosen up front.
- **Resumes:** model and effort are passed on every resume.
- **Reading results:** stream-json with `--verbose`; `is_error` is trusted
  over `subtype`; a later wake-up result does not hide the report; and the
  limit check reads only the message fields.
- **Moving sessions:** the transcript is moved together with its sidecar
  directory.
- **Process identity:** a pid is checked against its start time.

It drops:

- **Project add-ons:** the database-per-lane module, the worktree pool and the
  `gum` board (`relay view status` and `--json` replace the board).
- **Scheduling extras:** per-model caps.

`--after` is kept, with one change: a dependency that did not finish blocks
its dependents (`relay queue release` frees them) instead of freeing them.

Like lanes, relay never pushes or merges. Landing work on your main branch
stays with your project's gate.

## In TheGrid's words

relay is built to move onto TheGrid, so it uses TheGrid's model where it can.

| relay | TheGrid | how it differs |
| --- | --- | --- |
| system (queue, dispatch, quota, land) | System | A process, not a step machine; it changes state only by appending events. |
| a session attempt | System | One per attempt at one incarnation of a job, with its own file. |
| event file, `bus/<…>.ndjson` | Coupling, with the system's Port as its one source | Readers are listeners; nothing else can write it. |
| `Source` | source Capability | Linear and unforgeable: only the owning module can make one. |
| event | message | A fact that happened (queued, dispatched, reaped); it never commands. |
| the fold, `world.json` | state, checkpoint | relay keeps the event files as the record and the checkpoint as a cache; TheGrid keeps state and may forget history. |
| doorbell, idle exit | wake, rest | Nothing runs while nothing is queued or running. |
| a retry, on any account | recovery | A new attempt writes a fresh file; the old runner lacks the capability to be heard. |
| reaped | reap | A runner that died without reporting. |
| forgotten (`land clean`) | end | The job leaves the state; its files stay as the record. |
| job, account | (none) | A unit of work, and a resource outside the model. |

## Development

```sh
cargo test                          # unit tests, plus end-to-end flows with stub claude and omp in bwrap
cargo clippy --all-targets          # pedantic lints are on
RELAY_TEST_KEEP=1 cargo test --test flow   # keep the flow's files for a post-mortem
```
