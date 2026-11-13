# relay

Queue coding-agent tasks for any git repository and let them run unattended:
each task is a headless session of an agent harness (Claude Code, omp or puku-cli)
inside a bubblewrap sandbox, on whichever account of whichever
harness has the most quota to spare. When an account runs out mid-task, the
session moves on: to another account of the same harness with its transcript,
or afresh to another harness.

`relay ask` is the other half: a one-shot, read-only call to any harness, now,
in your own account ([the skill](../.agents/skills/relay/SKILL.md)).

```sh
relay queue add fix-parser --prompt briefs/fix-parser.md --model claude=claude-sonnet-5-5
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
  (`~/.omp/profiles/<name>`), `puku-cli` per config dir and sign-in
  (`PUKU_CONFIG_DIR=~/.puku-cli.work APP_NAME=pukucode.work puku-cli auth login`). puku also
  needs `node`, which it runs on anyway, for its quota (found as a program is, below).
- Rust 1.99 to build it (the toolchain file pins it).

```sh
cargo install --path relay --locked      # puts `relay` in ~/.cargo/bin (--root ~/.local: ~/.local/bin)
cp relay/relay.example.toml ~/work/myproject/relay.toml   # then edit `name`
cd ~/work/myproject && relay probe       # checks the sandbox; starts no model session
```

relay finds its config from `--config`, then `$RELAY_CONFIG`, then the nearest
`relay.toml` at or above the current directory. [`relay.example.toml`](relay.example.toml)
lists every key with its default. Unknown keys are errors. The file holds
per-machine choices (accounts, limits), so a project shared across machines
usually gitignores it.

`name` names the project's queue: its state lives under `$XDG_STATE_HOME/relay/<name>`
and its clones under `$XDG_DATA_HOME/relay/<name>`. The first command to open a
project records its repository's root there (in the file `repo`), and relay then
refuses a config with that `name` but another repository, naming both, so two
checkouts (or worktrees) of one project each need their own `name`. If the
repository moved, remove that `repo` file and the next command records the new root.

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
the dispatcher awake. So does every job queued behind it, however deep the chain
(`waits for b (blocked)`), and jobs that wait for each other (`a cycle`); `view result --wait` on it exits 4 with that line rather
than wait forever. Queue the dependency again (or `queue resume` it), or
`relay queue release NAME [--from a]`, which **starts the job now** (once a seat is free): it
drops the waits and does not cancel anything. To withdraw the job instead, `relay queue dequeue NAME`.
Only existing jobs can be named, so there are no cycles.

**Where a build job starts.** A build job starts from the repository's `HEAD`,
unless it waits for build jobs (`--after`) or forks one (`--fork`): then its
clone starts from that job's branch tip, so it holds the work it depends on
before anyone lands it. A read-only job in between passes its own
dependencies on: a build after a review of a build starts from the first
build's branch. With several such jobs, it starts from the tip that
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
spots, and independence is what a review is for. Queue reviews fresh: relay
refuses a read-only job that forks a build job (exit 5), from `queue add` or
from a decision.

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
| `relay queue add NAME ... --check CMD [--check-red-first]` | Declare the bar: a command that must pass in the job's clone before its run is accepted ([the bar](#the-bar)) |
| `relay queue add NAME ... --review-of JOB` | A read-only review of build job JOB, after it, with its branch fetched for the reviewer; its verdict is kept on JOB. JOB has one live review chain: this replaces the current one ([reviews](#reviews)) |
| `relay queue resume JOB ... --review-brief FILE` | Also replace the brief of the review chained to JOB: its reviewer is told FILE's text when it judges the branch again (exit 4 when JOB has no chain) |
| `relay queue unreview JOB` | Detach the review chained to JOB, with no replacement: it no longer re-reviews or resumes JOB (exit 4 when none is chained) |
| `relay queue add NAME ... --fork JOB` | Start as a fork of JOB's session, from its branch when it is a build job ([forks](#what-a-job-is)) |
| `relay decide list [--wait [--timeout S]]` / `relay decide answer ID --action A ...` | Decisions waiting for the orchestrator / answer one ([hooks](#hooks)) |
| `relay queue release NAME [--from A,...]` | Start a queued job now, without waiting for some or all of its `--after` jobs to finish (not a cancel: `dequeue` withdraws it). A job the [disk floor](#disk) holds is freed of those jobs too, and starts when there is room |
| `relay queue dequeue NAME` / `relay queue stop NAME` | Withdraw a queued job / end a running one (or a job the [disk floor](#disk) holds, which then stays stopped). A dequeued job never started, so its name is free: adding it again replaces every file of the old job (brief, logs, decision requests), and the event log keeps the history |
| `relay view status [--all] [--everywhere]` | Jobs, decisions waiting and accounts. `--everywhere` lists every project on this machine, each job with its project; outside a project that is what it shows. With `--json`, the whole folded state (every project's, by name, with `--everywhere`) |
| `relay view result NAME [--wait]` | Final report, status, cost, branch, log path; `--wait` first blocks until the job ends and its runner has exited (it may re-review or wake the dispatcher after the end), and exits 4 at once for a job that never will on its own (`waits for a (failed)`) |
| `relay view watch [--all] [--everywhere]` | The status live in a terminal UI, every pane scrolling on its own ([the board](#the-board)); `u` swaps it for the quota rows ([usage](#usage)), `c` there for the chart |
| `relay view events` | The merged event log |
| `relay view stats [--since DURATION]` | Per profile and model: jobs by outcome, spend, retries, decisions, checks, reviews, waits, and each account's share ([stats](#stats)) |
| `relay view usage [--meter NAME] [--account H:L] [--harness H] [--since DURATION] [--chart] [--watch]` | Each account's quota as a row: gauge, sparkline, pace, and what the pace comes to by the reset ([usage](#usage)); `--chart` the line chart over time instead; inside a project, its accounts from every project's readings; outside one, every account. `--watch` opens `view watch` on the rows, redrawn as readings land |
| `relay quota show` / `refresh` / `enable L` / `disable L` | Account room for the configured model, with a `for <model>` sub-row for each other model a queued or running job asks of it, judged as the dispatcher judges it (a full account is `busy`, not `ready`); `show` starts a reading of each stale account and says `refreshing`. A fresh reading (one under way anywhere on the machine is waited for); switches |
| `relay land fetch NAME` / `land clean NAME` | Copy the job's branch into your repository (no checkout), noting when no review has accepted its tip / delete its clone and records |
| `relay land gc [--dry-run]` | Remove what finished jobs no longer need: build outputs, old clones, old logs ([disk](#disk)) |
| `relay devshell warm` | Enter `session.env_shell` now and record it for the sessions; the dispatcher does this itself ([the development environment](#the-development-environment)) |
| `relay probe [--account H:L]` | Sandbox self-test for one account of each harness, with controls |
| `relay ask AGENT MODE ...` | One-shot read-only call, outside the queue ([the skill](../.agents/skills/relay/SKILL.md)) |

Every command takes `--json`: one JSON value on stdout, and a failure as
`{"error", "kind"}` on stderr. The exit code says which kind it was, with or
without `--json`: 2 usage, 3 not found, 4 the job is in the wrong state, 5
refused by a rule (a banned model, a disabled harness or one whose program is not found, the `ask` contract), 1
anything else. An agent driving relay can branch on those, never on wording.

Nothing needs starting by hand. Every command that changes something wakes the
dispatcher, which exits on its own a minute after the queue empties and no
runner holds a job (a job `checking` at a parked decision counts: if its runner
dies there, the dispatcher is still awake to reap it).

Whoever wakes it chooses nothing of its environment. The dispatcher, and everything it
starts (runners, gc, the quota reader, hooks, sessions), gets one built from the config
and this machine: `HOME`, the user and login shell from `/etc/passwd`, the state and data
roots this relay serves, the policy file the config was merged with, `LANG=C.UTF-8`, a
`PATH` (below), and `[session] env` over all of them, except that relay's own processes
keep this relay's `HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME`, `RELAY_POLICY` and
`RELAY_CONFIG` whatever `[session] env` says (a session does get the ones set there), so a
dispatcher always serves the queue it was woken for. `RELAY_CONFIG` is always the config that
relay serves (the one each child is also given as `--config`), so a `command:` hook that calls
`relay` back without `--config` reaches the same project, even one whose config lives away from
its repository. No `TZ`, `GIT_DIR`, token or other
variable of the calling shell reaches it. With no `PATH` in `[session] env`, the `PATH` is
relay's own directory, `~/.local/bin`, `~/bin`, `~/.cargo/bin`, mise's shims
(`~/.local/share/mise/shims`), `~/.nix-profile/bin`, the system Nix profiles, each found
harness program's directory, then `/usr/local/bin`, `/usr/bin` and `/bin` (those that
exist). A job or `--check` that needs a tool from elsewhere, such as a devshell's, gets it
from `session.env_shell` or a `PATH` in `[session] env`.

Every session relay starts for a job (the job's own, a review's, and a decision `agent:<profile>`'s)
and every run of its acceptance check also gets four variables naming the job, a contract other tools
may rely on. relay sets them last, after `[session] env`, the development environment and
`sandbox.env_deny`, so nothing can override them (a `[session.env]` entry of the same
name is refused when the config loads):

| Variable | Value |
|---|---|
| `RELAY_PROJECT` | the project's name (`name` in `relay.toml`) |
| `RELAY_JOB` | the job's name |
| `RELAY_ATTEMPT` | the attempt number: `1` for the first run, counting each resume |
| `RELAY_PHASE` | `session` for the session, `check` for the check at the branch's tip, `check-base` for the check at the commit the branch started from, `decision` for a decision agent asked about the job (a `command:` hook is neither a session nor a check, and gets none of these) |

A check can use them to label what it creates (a database per run, say), telling the session's resources
from the tip's and the base's.

A running dispatcher keeps the config it started with, so it checks the project's
`relay.toml` and the policy file (the sources `relay config show` merges) on every
pass, as it does its own binary. When either has changed and the result still
loads, it hands over to a new dispatcher that reads it; a file that does not load
(a half-saved edit) changes nothing until it changes again. An edit rings no bell,
so it is noticed on the next pass, at most five minutes later; any queue command
makes that pass at once.

## The board

`relay view watch` draws `view status` live, redrawn as events land in any
project: the jobs, any decisions waiting, the selected job, and the accounts.

- **Panes.** Each pane scrolls on its own and shows a scrollbar and its position
  (`10-29 of 164`; the jobs say `14 of 47`) when it does not all fit. Tab and
  Shift-Tab (or → ← and l h) move the focus, shown by a thick border; ↑ ↓ (k j),
  PgUp PgDn (Space, Ctrl-U Ctrl-D) and Home End (g G) act on the focused pane, and
  in the jobs pane move the selection. The wheel scrolls the pane under it; a
  click selects a job or focuses a pane; each key in the bottom line can be clicked.
  The job pane counts wrapped rows, so End shows the last line of any answer.
- **The job pane** starts with the whole task, then where the job stands, what it
  runs as (the profile, the full model id and the effort), what it waits for or why it ended, its checks, reviews and notes, any
  decision waiting on it, what its session is doing now, and its answer with
  headings bold and markdown markers dropped. Enter hands it to `$PAGER` (`less` by
  default) and takes the terminal back after. Only the selected job's log and
  answer are read, the answer again only when its file changes.
- **Every project.** `p` (or `--everywhere`) lists every project's jobs, with a
  project column and each project's counts in the title; outside a project (no
  `--config`, `$RELAY_CONFIG` or `relay.toml` above) the board shows every project, and
  its accounts pane is machine-wide: every account on the machine, with what all projects
  share (sign-in, limits, the latest reading, sessions running), and none called `ready`,
  since the floor, seats, exclusions and `quota disable` that decide it are each project's.
  Other projects are read, never written: a job's row and pane come from its own
  project's fold and jobs directory, so what holds another project's queued job
  (which only its own config can judge) is left out; its dependencies still show.
- **Why a job waits.** A queued job's row and pane say what holds it, judged as the
  dispatcher judges it: what it waits for, else its profile's limit (not once a
  `held` decision forced it past), free disk, the [development environment](#the-development-environment)
  while it warms (`dev shell warming (12m)`), or every account's cause
  (`no account has room (claude:a busy (1 of 1 seat); claude:b reading is stale)`).
  A retried or requeued job has no account until its next start, so its account
  column says `-` and its model column lists every harness it may run on.
- **Accounts.** A row per account, and per provider for omp (`omp:default ·
  openai-codex`, the one its configured model draws on first), with a column per
  window, shortest first (`5 hours`, `7 days`, `month`; another length, or `other`,
  when a harness reports one), so one window reads down across every account; a column
  shows only when some row has a meter of it. Then `room/day`, `run` and the state,
  last. A meter that counts one model or group alone (claude's `Fable` week,
  antigravity's `Gemini`) is on a `└ Fable` sub-row under its account, in the same
  columns; a window a row has no meter of is `—`.

  ```
                                      5 hours                   7 days                    month      room/day  run  state
    ▸ claude:tj.pro                   █████▎░░░░│░  44%* 10m    ████▌░░░░░│░  38%  2d16h  —          23%/d     0    ready
      claude:studyac17                —                         ███▍░░░░░░│░  28%* 5d14h  —          13%/d     0    ready
        └ Fable                                                 █▍░░░░░░░░│░  11%  5d14h
      omp:default · openai-codex      ░░░░░░░░░░│░   0%  5h     ██▎░░░░░░░│░  19%* 5d4h   —          16%/d     0    ready
      omp:default · minimax-code      ▋░░░░░░░░░│░   5%  2h     ▌░░░░░░░░░│░   4%  5d     —
  ```

  Each meter is a gauge: a bar in eighths with `│` at that meter's floor, how full, and
  the time to its reset (`6d5h`). The meter that decides is bold (`*` in `view status`,
  which is never coloured); on the board a gauge is green, yellow within 10 points of
  its floor and red at it. `room/day` is the account's room, which dispatch ranks
  shared among the sessions in `run` and the next one, and `▸` the one the next start
  would take, also named in the title with the floors. A stale reading says how old it
  is (`stale (read 3h ago)`), or, when the reads since gave nothing, why (`reading timed
  out after 180 s (3 tries since Nov 12 18:08 +06); read 3h ago`). Who a home is signed in as is in `--json` (`identity`);
  twins say `same sign-in as`. Each meter in `--json` (`accounts[].meters`) says what
  it measures beside its level: `window` (`fiveHours`, `week`, `month`, or `{"other":
  hours}`), and `provider` and `scope` when it has them. A narrow window leaves the
  emails out, then shrinks the bars (to five cells), then drops them and keeps the
  numbers (`44%* 10m`), before it cuts a line; a row never wraps. `view watch` does the
  same per column: a gauge with no room for a five-cell bar shows its numbers alone,
  whole, before a name is cut.
- **Readings.** Showing the accounts in a project (`view status`, `view watch`, `quota
  show`) starts a reading of each stale one in the background, and the accounts title
  says `refreshing claude:a, …` until it lands. `view status --json` does too, with the
  accounts in a top-level `refreshing` list beside the folded state (absent when none). The machine-wide pane outside every
  project starts none: a reading is taken by a project's rules.
- **The quota rows.** `u` swaps the board for the rows of [`view usage`](#usage), drawn
  by the same code, and `u` again brings the board back; `c` there turns the rows into the
  line chart and back. They refold as quota readings
  land on any project's bus, for the same accounts `view usage` shows (inside a project
  its own; outside one, every account). While they show, only `q`, `?`, `u` and `c` do anything,
  and it reads the buses and nothing else: no reading is started (the board's accounts pane
  does that), and nothing is written. `relay view usage --watch` opens on the rows,
  `--watch --chart` on the chart. A window too short for every row says how many it leaves out.
- **What stays put.** The selection follows a job by name as rows move; a failed
  read keeps the last good board with the cause in red in the title, and the next
  read retries; columns are a fixed set (project when every project shows, job, state,
  account, as, **model**, since, spent, disk, task), each capped and cut with `…`; an empty list
  says what `a` (older jobs) or `p` would show. `?` lists the keys and colours.
- **The terminal.** `q`, Esc, Ctrl-C, SIGTERM, SIGHUP and SIGINT give it back; the
  board takes the mouse, so hold Shift to select text. A reinstalled relay takes
  the window over by itself.

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
`max_running` and `max_per_day`, all optional; and declares the bar, the review
independence and the escalation of its jobs (`check`, `check_red_first`,
`independent`, `escalate_to`, `escalate_after`, below). `relay queue add NAME --profile P`
fills each field the command line does not set (`--no-readonly` turns a profile's
read-only off); explicit flags win. The model reaches the same banned-model check
as one given by hand, at queue and at start. The job records its profile, and it
stays with the job across `queue resume`.

Two limits count per profile. `max_running` is the most of its jobs with a
runner at once. `max_per_day` is the most of its sessions to start in a rolling
24 hours, counted from `dispatched` events on the bus, so cleaning a job away
gives nothing back (a retry on another account is a start too). A job they hold
stays queued with the reason on the board and in the dispatcher's log:
`profile bug-hunt: 3 started in 24h (max 3), next slot 20:02 +06`. A job no account
can take is held with every account's cause:
`no account has room (omp:default busy (1 of 1 seat on openai-codex), at floor: 100% openai-codex/5 hours, resets 19:36 +06)`.

Every time relay shows or logs is local time with its zone: `TZ` if set, else
`/etc/localtime` (a container's is often UTC, so set `TZ` there, e.g. `TZ=Etc/GMT-6`).

`relay profiles [--json]` prints each resolved profile with its note, which is
how a fresh orchestrator learns the policy. `relay config show` prints the merged
bans and profiles with `[policy]` or `[project]` after each setting. Neither creates
state, and both run anywhere: outside a project (no `--config`, `$RELAY_CONFIG` or
`relay.toml` above) they show the policy alone, checked as a project would check it
(`config show` prints `project  none`).
`relay view status` and `view watch` show a job's profile where it has one, else
the model it names, and in a `model` column after it the model the job runs, short
(`claude-opus-5-5` is `opus-5.5`, `openai-codex/gpt-6-luna` is `luna`): the harness
of its account while it runs (or once it has run, while that harness is still one of the job's), with the model its dispatch recorded, else every harness it may run on, comma-separated (a job back in the queue has no account until it starts again); a harness the job names no model on shows its configured one (another project's queued job shows only the models it names);
the job pane has the full id and the effort. `--json` carries both (`profile`, `models`, and `model` and `provider`: what its latest dispatch resolved), and each job's `kind` (`build`, `read` or `review`), `readonly` and `acceptance` (`none`, a `check`, a `check-red-first` or the job it is a `review-of`).

## Hooks

relay meets a few moments where a judgment beats a fixed rule. At each
**decision point** it asks a chain of **handlers**, in order, and carries out
only an action from that point's closed set, after checking it against policy:

| point | when | actions |
| --- | --- | --- |
| `cap` | a build job's run hits its spend cap | `continue {budget}`, `redirect {message, budget}`, `stop {why}` |
| `ended` | a run ends done or failed (an error, silence, no FINAL after the follow-up) | `accept`, `resume {message, budget}`, `requeue {profile}`, `follow-up {brief, profile, after, fork, name}`, `stop {why}`, and for a review `review {verdict, reason, message}` |
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
seat_wait_minutes = 30              # an `agent:` handler waits this long for an account, then defers
command_timeout_seconds = 120
allow_force = false                 # whether `held` may answer `force`
max_decisions = 10                  # decisions that may act on one job

[limits]
job_ceiling_usd = 50                # required once any chain is set
```

Every handler may also answer `defer`, which passes the decision to the next.
When all defer, relay does what it does without hooks: at `cap` the job waits
for a human (`failed`, resume with `--budget`), at `ended` the run's outcome
stands, at `held` the job waits, at `disk` the job is stopped (held, when the floor did it: [disk](#disk)).

- `rule`: relay's own checks, free and deterministic. At `cap`, no new commit
  and no changed file since the last checkpoint is `stop`. At `ended`, a done
  run whose FINAL says `status: done` is `accept`, and a run that went silent
  is resumed once in the same session. It also judges what a job declares: a
  failed [acceptance check](#the-bar) is `resume` (unless it repeats the last one
  at the same commit: then it defers), a review's report gives its
  [verdict](#reviews), and enough failed ends [escalate](#escalation) the job.
  Otherwise it defers. A job that declares any of these has an `ended` chain
  of `["rule"]` when `hooks.ended` is unset, so it costs nothing to leave unset.
- `agent:<profile>`: a short read-only session on that profile's harness, model
  and budget, on another account than the worker's where one is free. Its seat is
  chosen as the dispatcher chooses a job's, by the same judgment and under the same
  machine-wide lock (never waited on: a handler that finds it busy tries again within its own
  wait, or a few seconds when it cannot wait), and booked on the bus (`seated`, then `unseated` once the
  session has ended), so every dispatcher and every other decision counts it as a
  session like any: with `per_account = 1`, a job queued meanwhile never joins it on
  its account, and two decisions at once take different accounts or one waits. A seat is
  free when the account has room under `limits.per_account` (counting every
  project's sessions) and a quota reading that is fresh and above the floor; the
  worker's own account serves when no other does. A worker at `cap` or `ended` holds no seat
  while it is asked about; one at `disk` still runs, so it keeps its seat. At `cap` and `ended` the
  handler waits up to `hooks.seat_wait_minutes` for a seat (taking a new quota
  reading where one is old), then defers with a reason that names that bound;
  at `held` and `disk` it does not wait, since neither can: a held job is asked
  again later, and a disk nearly full stops the job (resumable) at once. Its prompt
  holds the point, the brief, the branch since the last checkpoint, the worker's
  last message, the job's earlier decisions and the JSON shape of each allowed
  action. `limits.checker` with `budget_growth` above 1 is shorthand for
  `cap = ["rule", "agent:checker"]` (see [checkpoints](#checkpoints)).
  Its session, in a process group of its own, is watched until it has exited,
  not only until its output closes, and is killed with its group once its
  decision is withdrawn or goes stale, or a limit passes. Every handler and
  session runs under a small supervisor (`relay supervise`) that leads its
  process group and ends everything the handler started when the handler exits, when it
  is stopped, or within moments of its runner ending (`queue stop`, killed, crashed). It is
  a subreaper, so what leaves its group (a `setsid` child, a sandbox's processes) comes
  back to it, and it exits only once it has ended and reaped every one of them; nothing
  is recorded and no stored pid is signalled.
- `command:<argv>`: runs the program (argv split on spaces, with no quoting) on
  the host with the request as JSON on stdin, and reads one JSON object from
  stdout: `{"action": "continue", "budget": 20, "why": "..."}` (`cost_usd`
  optional, never counted below zero). It runs without the variables
  `sandbox.env_deny` names, is killed 5 s after `command_timeout_seconds` if it
  ignores the TERM, and may print at most 64 KiB. It runs in a process group of
  its own, killed whole within a second once its decision is withdrawn or goes
  stale, and a second after that grace if anything in it (a helper it left in the
  background) still holds its output open: it is done only once it has exited
  and its output is closed. A malformed answer, a failure or a timeout is a `defer`.
- `orchestrator`: parks the request on the bus. `relay decide list --wait`
  blocks on the doorbell until a request is waiting and prints it (`--json`:
  each with its full request); `relay decide answer ID --action A [--message M]
  [--budget USD] [--profile P] [--brief TEXT] [--why W] ...` answers it. Past
  `orchestrator_timeout_minutes` the chain moves on, and an answer recorded
  later is refused (exit 4) and changes nothing. A decision is about one attempt
  of its job: once that attempt has moved on (`queue resume`, a new run, the job
  reaped or ended) or the process waiting for the answer (the job's runner, or
  `decide held`) has died, an answer is refused (exit 4, saying which), `decide
  list` no longer shows it, and the runner stops waiting and asks no further
  handler. A handler that answers after that (a `command:` or `agent:` still
  running, an answer read late) is recorded as `defer` (`withdrawn: ...`) with
  what it cost. Whatever was decided is carried out under the lock `queue resume`
  takes, and only while the job is still at the attempt it was asked about: a
  `follow-up`, `resume`, `requeue` or review verdict never acts on a job that
  has moved on. A runner waiting for an answer shows `checking` and holds no seat. `relay view status` lists waiting
  decisions in their own section, so a human sees them too.

**Safety.** A worker's report and diff reach an `agent` or `command` handler,
so a worker can try to talk its judge into more money. relay bounds what any
answer can do, and checks every action before acting:

- the point allows it, and fewer than `max_decisions` decisions have acted on
  this job (counted afresh when it is queued anew);
- a budget is positive, and the cap the next run will get (the one named, or
  the job's cap when none is) keeps the job under `limits.job_ceiling_usd`,
  which every `[hooks]` setup must name;
- an `agent` or `command` grants at most the point's next cap (at `cap`, the
  last cap times `budget_growth`; at `ended`, the job's cap); a larger budget is
  cut to it. Only the orchestrator names more;
- a `follow-up` of a job that is itself a follow-up comes only from the
  orchestrator, so automated chains stop one deep;
- a profile exists, its model is not banned, and its `max_running` and
  `max_per_day` have room. A follow-up naming no profile takes its parent's,
  and that profile's limits count it. A `reroute` naming none keeps the job's,
  whose limits dispatch applies as before;
- a rerouted harness is enabled and its model is not banned; `force` is allowed;
- a fork's harness can fork, and a read-only job does not fork a build job.

The prompt marks the worker's words as untrusted data and bounds them. A
handler's action that fails is recorded with the refusal and counts as
`defer`; `decide answer` refuses it at once (exit 5). A `held` decision acts
only if the job is still queued as it was when asked, and one job is asked
about at most once per queueing and once per five minutes. Hooks never land,
push or delete: those stay with you.

Every decision is on the bus: `decision-requested {id, job, point}`, `seated
{id, account, model, provider, lock}` and `unseated {id}` around an `agent:` handler's
session (its seat counts until `unseated`, even if the job is cleaned away meanwhile, and only
while something holds its `lock` file: the process that asked, and the session's supervisor, which
holds it until it has ended and reaped the session's whole tree, so a session outliving a killed
runner for a moment still holds its seat; a booking for a decision already cleaned away is
refused), `parked
{id, until}` while it waits for the orchestrator, `answered {id, action, why}`
from `decide answer` (heard only before `until`), and one `decided {id,
handler, action, why, cost_usd, refused}` per handler asked, written by the
process that runs the chain, so the record shows what was carried out. A handler's spend
counts toward its job's total. `relay view status --json` has every decision
under `decisions`. A `follow-up` is queued like `queue add` (by default
`--after` the job, named `<job>-next`), with its `parent` and `depth`; `requeue` starts the job afresh under
the profile with its branch kept, and `reroute` and `force` are queue events.

## The bar

A brief says what to do; the bar says what must be true afterwards, and it is
judged by relay, not by the session that did the work. It is **declared**: on the
line, `queue add NAME --check "cargo nextest run --no-tests=fail"`, or as a
profile's default, `[profiles.P] check = "..."` (for build jobs; a line's own
`--check` wins). It is stored on the queued record (`acceptance` in `--json`), so
changing a check means queuing a new job. A job that declares none pays nothing.

```toml
[profiles.builder]
check = "cargo nextest run --no-tests=fail"
check_red_first = true     # the profile's own check must also fail at the job's base commit
```

When a run ends `done`, the `ended` point's `rule` handler has the check run, and
relay records `bar-checked {job, commit, exit, tail}`, the tail being the end of the
output, bounded as worker output is before it reaches a handler, and told to the
handlers as untrusted.

- **On the committed tip.** The check judges what would land, so it runs on a clean
  checkout of the branch's `HEAD`, read once and named in the record: files the
  session left uncommitted or untracked are not there for it. The checkout is a fresh
  clone with a configuration of its own, so the session's `.git/config` cannot name a
  program for the host to run. Because it is fresh, a build starts from nothing.
  What the check leaves behind never blocks the next one: its directory (the checkout and
  its `TMPDIR`) is cleared before each run and after it, and a directory a test set to mode
  `0o000` is made owner-readable and removed (a symlink in it is unlinked, never followed).
  A directory that still cannot be cleared is logged on the job's events as
  `cleanup-failed` with its path, and the check that needed it is `check-could-not-run`
  ("could not clear the check directory ..."), not a failed checkout.
- **In the session's sandbox plan** (the repository read-only, the same network and
  limits), under `hooks.command_timeout_seconds` (raise it for a long suite), but
  with the session's clone and **its account out of sight**: the check cannot read
  or change the sign-in the session used. A development environment
  (`session.env_shell`) is the one its warm-up recorded from the project's repository, not
  from files the session could have edited.
- **Pass:** the run may be accepted. **A run whose check did not pass is never
  `done`**, whoever decided: after the chain has answered, relay ends it `failed` with
  the check's result as the reason. That is the one enforcement; it is also told: an
  `accept` of a run whose check is red is refused where it is given (`decide answer`
  exits 5 with the reason and the decision keeps waiting; a handler in a chain is recorded
  as refused and the chain goes on to the next one).
- **Fail** (the command ran and failed, and not only as it does at the base): `rule`
  answers `resume` with the failing tail as the message, so the same session fixes what the check found and ends again,
  and the check runs again. This is bounded by `hooks.max_decisions` and
  `limits.job_ceiling_usd`; when a resume is refused the job ends `failed`, its reason
  naming the check. A fail is not a failed *end*, so it counts toward
  [escalation](#escalation) only once the job itself ends failed.
- **No progress:** a fail at the same commit as the job's previous check, which failed
  the same way (the same exit statuses, at the tip and at the base, and the same failing
  tests, read as for base-red, where both runs name some; where either names none, its output
  unread or its failure not a test's, the exit statuses alone decide; not the output, which
  carries timings), is not resumed
  again. The check runs on the committed tip, so a session that committed nothing since
  has nothing new for it to judge, whatever it changed in its working tree. `rule` defers
  with `no progress: same commit, same check failure`, and the rest of the `ended` chain
  (the orchestrator, an agent or a command) decides; with none left the job ends
  `failed`, its reason saying so. A different fail at the same commit (another test,
  another exit) is a check that says different things of the same code, which is news, so
  it is resumed as above; the next fail that repeats it is not. The record marks it
  (`no_progress` under `bars`), and a resumed run that has changed nothing since that
  commit does not grow its cap either ([checkpoints](#checkpoints)).
- **Base-red:** a check that fails at the tip is run again at the job's base (the
  commit its branch started from, as the revert control does). If it fails there too,
  and every test failing at the tip fails at the base, the failure was there before
  the job: relay records `base-red` with the failing tests' names, does not resume the
  session (a brief's "stop if a test you did not touch fails" is not overridden), does
  not accept, and stops the job with its commits as they are, for you to look at. The
  tests are compared by what the whole output names (read before it is cut to its end):
  `cargo test`'s `test NAME ... FAILED` lines and the names under `failures:`, and
  `cargo nextest`'s `FAIL`, `TIMEOUT`, `SIG…` and `ABORT` lines, whole
  (a doctest's `src/lib.rs - f (line 3)` counts), each after the test target it ran in
  (`tests/flow.rs (flow): NAME`), since a name is only unique within one. An output with
  a line over 64 KiB or more than 1000 failing tests is not compared: its fails are the
  job's. When neither output names a test (a
  formatter, a build that fails or a runner relay does not read), failing at the base
  is enough: a wrong `base-red` stops the job for you, where a wrong resume sets the
  worker fixing what it did not break; when only one does,
  they failed differently and the failure is the job's. A new failing test is the job's:
  it is resumed as above. A timeout or failed checkout at the base is
  `check-could-not-run`, never base-red, and so is a base run that exited a code the project
  declares (below). A job with a revert control expects its base to fail, so its fails are
  always the job's.
- **Could not run:** a command that timed out (124), was not found or not runnable
  (126, 127), was killed, or whose checkout failed said nothing about the code. relay
  records `check-could-not-run`, does not accept, and stops the job for you to look
  at: a broken check is not the session's to fix. This holds at the tip and at the
  base: a check that fails at the tip and could not run at the base ends so, never
  base-red, and the record's `tail` begins with why, kept whole however long the output (`the check could not run: it timed
  out (exit 124, after 120 s: hooks.command_timeout_seconds)`; `at the base commit, …` for
  the base), from the exit status and the timeout relay applied. relay never reads the
  output's text for this. A tool that refuses to run exits with an ordinary failure code
  (mise's untrusted-config refusal exits 1), which relay cannot tell from a failing check;
  the project says which code its check uses: `hooks.check_could_not_run_exit_codes = [125]`
  (codes 1-255; 124-127, 137 and 143 always count), and the check ends with that code where
  it knows the environment refused it (`mise trust -q || exit 125`, or
  `some-tool --version || exit 125` before the suite). A declared code is the project's
  word: it also turns a real failure that exits it into `check-could-not-run`.
- **Revert control:** `--check-red-first` (or `check_red_first = true` beside the
  profile's `check`) also runs the check at the commit the job's branch started from
  (its base), and requires it to **fail** there. Only a real failure counts: a
  timeout, a missing command or a failed checkout at the base is `check-could-not-run`,
  never a red. A check that passes without the change proves nothing: relay records
  `check-proves-nothing`, does not accept, and stops the job. A check that filters
  tests passes when the filter matches none, which is exactly this case: give it a
  "no tests is a failure" flag, such as `--no-tests=fail` for nextest.
- **A ceiling.** A check resumes the session, so `queue add` refuses a job that
  declares one (or a review, or whose profile escalates) unless `limits.job_ceiling_usd`
  is set: what such a job may spend is then bounded for sure, not only by
  `max_decisions`.

`view status` shows the latest result (`check pass`, `check base-red: report::tests::reset`,
`check fail: flow (no progress: same commit, same check failure)`), `view result` the exit
statuses and commit, and `--json` every run under `bars`, with the failing tests a fail or a
base-red names (only the latest keeps its tail).

**Older relays.** A job that declares an acceptance carries a dependency on a job
named `needs-relay-acceptance` that does not exist: a relay that does not know what a
check or a review is (a dispatcher from an older build sharing the bus) sees a job
waiting for a job that is gone, and does not start it, where it would otherwise run
it unchecked. This relay drops that name when it folds, so nothing shows of it, and
refuses job names that start with `needs-relay-`.

## Reviews

`queue add NAME --review-of JOB` is sugar for a read-only job, `--after JOB`, with
JOB's branch made visible. A read-only job mounts your repository read-only, and a
build job's branch lives in the build's clone, so a plain review cannot see it.
Before the reviewer starts, relay fetches the branch into your repository as
`refs/relay/review/<NAME>` (as `land fetch` copies it, but not onto your branches),
and the prompt's preamble gives the reviewer the ref, its commit and the diff base
(the commit the builder started from). The preamble comes with **every** start and
resume of the review, with the commit as the ref stands then, so a resumed review is
told the exact commit to judge and a verdict never names one it did not see. The ref
goes with the review on `land clean`, and `land gc` removes the refs of reviews that
are gone or ended `gc.outputs_after_hours` ago.
JOB must be a build job; a review never forks it, and is never refused as a repeat
question (the branch can move under one brief).

**Verdicts.** The reviewer is told to end its FINAL section with `verdict: accept` or
`verdict: reject`, and for a reject `reason: <code>` and its findings in `notes`. The
`ended` point's `rule` reads them; an `agent`, `command` or the orchestrator can
answer `review {verdict, reason, message}` instead (`decide answer ID --action
review --verdict reject --reason test-failure --message ...`). A reject carries a
coded reason: `test-failure`, `behavior-mismatch`, `insufficient-evidence`,
`stale-head` or `other`; an accept carries none. relay records `reviewed {job, of,
commit, verdict, reason, family, stale_head}`; the fold keeps `verdicts` on the reviewed job, each
with the reviewer, the commit it judged and its reason, and `view status` shows the
verdicts for the latest reviewed commit only (`review by rev: reject: test-failure`),
so an earlier commit's reject is history, not the state.

- **A reject resumes the builder's session** with the reviewer's findings, as `queue
  resume` would, while the builder is not working, no more than `hooks.max_decisions`
  reviews have rejected it (later rejects are recorded, and say they resumed nothing),
  and another run keeps it under `limits.job_ceiling_usd`.
- **A builder that answers a reject is judged again.** When that run ends `done`, relay
  resumes the reviewer whose latest verdict was the reject, on the new tip: its ref is
  fetched again and the preamble names the new commit, so the board does not keep a
  stale verdict. The same bounds apply (the job's ceiling and a banned model; and
  `hooks.max_decisions`, which caps the decisions on a job: a reviewer that has had
  its share is not resumed, since its verdict could not be recorded), so a reviewer
  and builder that keep rejecting each other stop there, and the board shows the last
  recorded verdict. A builder that fails is not re-reviewed. A reviewer that cannot be
  resumed is left as it was; queue another review. If the builder finishes before the
  review is marked ended, the review's own end asks again. A re-review is only asked
  once the builder is not working, so it always judges the builder's current head.
- **A job has one live review chain.** A new `queue add --review-of JOB` replaces the
  chain JOB has: the old reviewer no longer re-reviews JOB or resumes it, and the board
  says so (`reviews build (superseded by rev2)`; `unchained {by}` in `view status
  --json`). `queue unreview JOB` detaches the chain with no replacement
  (`(detached)`). Its verdicts leave the board's `verdicts` but stay in the event log
  (`reviewed`, `unchained`), so the replacement's rejects count toward `hooks.max_decisions` from nothing. A queued reviewer that has not run is stopped; one that is
  running finishes, and what it says acts on nothing. `queue resume JOB --review-brief
  FILE` replaces the chained reviewer's brief (its `prompt.md`) too: its next re-review
  is told the new brief, once, ahead of the usual message. Without the flag a resume
  changes nothing about the chain.
- **A head that moves during a review makes its verdict stale.** When a review ends
  and the builder's branch is no longer at the commit the review judged (someone
  resumed the builder meanwhile), relay records the verdict with `stale_head` set to
  the branch's head (`review by rev: stale-head`), does not resume the builder, and
  the chain reviews the new head as soon as the builder is done. A stale verdict never
  counts toward `hooks.max_decisions` on the builder.
- **Independence.** relay records the reviewer's model family (`anthropic`, `openai`,
  `google`, ...; a model it does not know is its provider). A review is refused on the
  builder's family only when the reviewed job's profile declares `independent =
  "family"`: at `queue add` (exit 5) when every harness it may use would do, else when
  its session starts, which then fails at once. Otherwise the family is recorded and
  allowed.
- **Landing.** `land fetch` prints `note: no review has accepted <tip>` when a review
  was asked of the job and none accepted its tip. It never blocks: landing stays
  your step.

## Stats

`relay view stats [--since DURATION] [--json]` is a read-only fold over the events
already on the bus (it writes none). It counts the jobs queued within the window
(`30m`, `12h`, `7d`, `2w`; default all), per profile and model, with:

- jobs by outcome (done, failed, stopped, open) and spend;
- retries (runs sent back to the queue) and resumes (by hand, by a decision, or the
  one more turn for a missing report);
- decisions by action (the action carried out, one per decision);
- checks by result (`pass`, `fail`, `check-proves-nothing`);
- reviews received by the jobs, accepts and rejects by reason;
- the median queue wait (queued or requeued, until a runner starts) and the median
  run time;
- each account's share of the runs and its spend, by label (`claude:work`), never by
  who it is signed in as.

A review is counted for the job it judged, so a profile's reject rate is how often
its work was sent back. Spend is the runs' and the decisions'; an account's spend is
its runs' alone.

## Usage

`relay view usage` is a table of each account's quota, printed inline at the
terminal's width so it stays in scrollback: one row per account and meter, the
one that resets soonest first, so the top row is the one to act on.

    claude:tj.pro     ██████████│░  91%  ▁▂▂▃▃▄▄▅▅▆▆▆▇▇  +13%/d  resets 19h    full now
    claude:studyac17  ██▊░░░░░░░│░  23%             ▂▂▃   +8%/d  resets 5d22h  ~72% at reset: 28% unused

- **Gauge.** The bar, floor mark (`│`) and percent of `quota show`, the
  same code, coloured on a terminal by how close it is to the floor (green, yellow
  within ten points, red at it). Then when it resets.
- **Sparkline.** The meter's level across the `--since` window, oldest at the left:
  the window cut into 14 equal buckets (fewer on a narrow terminal), each the level
  at its last reading, `▁` for 0 % up to `█` for 100 %. A bucket with no reading
  repeats the one before it; before the first reading, blank. A reset is the
  drop it is.
- **Pace.** How fast the meter is filling, in percent of the window per day: the
  rise across the readings of the last 24 hours, counted from the last reset
  in them. A reset is the meter's own statement: its reset time moving on by more
  than an hour. A level that dips a point between readings, or a reset time that
  shifts by a minute, is jitter and starts nothing; a pace never goes below `+0%/d`.
  Under two readings, or readings less than an hour apart, there is no pace (`—`).
- **Outlook.** The level at the reset if the pace holds, from the last reading, and
  the floor in force then (`floor_near_reset` as the reset nears). Where it reaches
  the floor first: `full in 1h at this pace` (`full now` when it is there), full
  being the floor, where dispatch stops. Where it does not: `~41% at reset: 59%
  unused`, the quota that will go to waste. No pace, no reset time, or a reset that has
  passed: `—`.
- **The label** is the account, its provider and the model or group its meter counts,
  as `quota show` puts them (`omp:default · google-antigravity └ Gemini`, `claude:a └
  Fable`), and the window when it is not the week's (`claude:a 5 hours`). Cut to fit,
  the provider gives way first, then the account, then the model or group, so the end
  that tells two rows apart stays; two meters never read alike. Two that would (claude's
  `seven_day` and `seven_day_overage_included` are both the account's week) carry their
  meter's name, `claude:a (seven_day)`, in the rows and the chart's legend alike.
- **Narrow.** The bar shrinks first (to five cells), then the sparkline (to six); then
  each outlook moves to a line under its row, and past that a line is cut.
- **`--chart`** prints the line chart instead: one line per account, time across
  (in your local zone), utilization 0–100 % up. A reset is a drop. The policy's
  `floor` is drawn across it (`┄`), and the `floor_near_reset` one (`╌`) when the
  policy sets it, so the room left is the gap. Each line ends in its id (`1`, `2`,
  ..., then `a`, `b`, ...), which needs no colour; under the chart, each id's
  account and meter with its last reading and when that meter resets, wrapped to
  the terminal's width, never cut. A narrow terminal gets shorter time labels, and
  a very narrow one none. Many accounts overlap on it, which is why it is not the default.

- **What it reads.** The readings already on the bus, as the project's fold takes
  them in: a quota command's (`quota` `reading` events) and a live session's
  (`session` `rate` events). It writes nothing (a harness's program it finds on `PATH` is used, not recorded
  in `programs.json`), and starts no reading. Every
  project's bus on this machine is read, in a project or outside one: an account
  is the machine's, so where a reading happened to be taken says nothing about
  whose it is. An account read by several projects is one row, its readings
  merged by time, one seen on two buses kept once.
- **Whose accounts.** Outside a project, every account. Inside one, only those it
  can dispatch to: the accounts its dispatcher considers, so its enabled
  harnesses' accounts without `accounts.exclude`'s. The heading says so
  (`tickify's accounts, quota since ...`). A project that rarely dispatches
  still charts its accounts, from the readings other projects took of them.
- **Which meter.** By default every account's week meters, as the harness that read
  them says (claude's all-models and Fable weeks, omp's `openai-codex/7 days`,
  `minimax-code/General 7 Day`, `google-antigravity/Gemini (Weekly)`), one row each;
  `--meter NAME` picks one by its name (`five_hour`), or every meter of a window by
  its column's heading (`5 hours`, `month`). A reading without that meter adds no
  point.
- **Which accounts and when.** `--account harness:label` (repeatable) and
  `--harness` (repeatable) narrow it; `--since` (`12h`, `7d`, `2w`; default `7d`)
  is the window.
- **Live.** `--watch` opens [`view watch`](#the-board) on the rows (`--watch --chart`
  on the chart) with the same `--meter`, `--account`, `--harness` and `--since`; it is
  redrawn as readings land, `c` switches between rows and chart, `u` to the job board
  and back, `q` quits. It cannot be combined with `--json`. Without `--watch`, the
  rows print once.
- **Piped**, it is plain text with no colour. `--json` gives the readings:
  `accounts.<account>.<meter>` is a list of `{t, utilization, resets_at}` (`t` in
  bus microseconds, `resets_at` in unix seconds, `utilization` a fraction), with
  `floor` and `floor_near_reset` beside it; and `rows`, one per row of the table in
  its order: `account`, `meter`, `window`, `provider`, `scope`, `readings`,
  `utilization`, `floor`, `resets_at`,
  `pace_per_day` (a fraction of the window), `projected_at_reset` (a fraction; null
  when it would be full first, or without a pace), `full_in_seconds` (null unless
  it would), `outlook` as printed and `sparkline`. `pace_window_hours` says how far
  back the pace looks.
- **Bare labels.** Old buses name some accounts by label alone (`adventor`).
  relay has no rule saying which harness such a label was on, so their readings
  are left out, and the view names them (`left out: ...`; `left_out` in `--json`).
- Meters only: never who an account is signed in as.

## Escalation

A profile can name the profile to move to when its jobs keep failing:

```toml
[profiles.cheap]
model = "claude-haiku-5-5"
escalate_to = "strong"
escalate_after = 2
```

`escalate_after` counts failed ends of one job (runs that ended `failed`, not retried
or resumed in place), since it was last requeued. An end the acceptance check turned
`failed` counts in that number, but the `rule` is asked about a run's own `failed` end
only, so escalation never fires for a job whose check keeps failing: the check's remedy
is the resume with its output, bounded by `max_decisions`, and then the job waits
`failed` for you or the orchestrator (`requeue`) to move it. A job whose profile escalates needs
`limits.job_ceiling_usd` (`queue add` refuses it otherwise), and a requeue is held to
it as a resume is. When the count is reached, the
`ended` point's `rule` answers `requeue {profile: "strong"}`: the job starts afresh
under `strong` with its branch kept, and the count starts again. The target
profile's `max_per_day`, its bans and `max_decisions` apply as for any `requeue`; a
refused one leaves the job `failed`. A first silence is still resumed in place before
it counts. The names are checked when the config loads: an `escalate_to` that is not a
profile, or that leads back to itself, is an error.

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
cache_copies_idle_days = 1   # an idle copy of the linked caches goes after this (see the sandbox)

[limits]
job_disk_gb = 40             # a running clone over this reaches the `disk` point (0: off)
min_free_disk_gb = 10        # also: a pass runs at once below this, and holds new starts
disk_resume_margin_gb = 3    # a held job resumes, and a queued one starts, this far over the floor
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
- **Scratch directories** (a job's `/tmp`, `<data dir>/<project>/scratch/<job>`) on the
  same rule as build outputs, for read-only jobs too, and any whose job no longer
  exists. A live job's, or one a queued job waits for, is never touched.
- **Whole clones** of `done` jobs after `clones_after_days`, landed or not; of
  failed or stopped jobs after `waiting_retain_days`, since those may be resumed.
  Every commit of the job's branch must be in the repository first (`land fetch`
  ran, or the tip is reachable from a repository ref); otherwise gc fetches it
  into `refs/relay/rescue/<job>` and says so. A clone with uncommitted work
  (build outputs aside) is kept, and the line says why. A job whose clone is gone
  is resumed from that rescue ref (else its fetched branch).
  The session wrote the clone's `.git/config`, so gc reads it as relay reads every
  clone after its session starts: with no hooks, `core.fsmonitor` or filter drivers,
  so nothing the clone names runs on the host.
- **Logs**: `<job>.log.ndjson` of a job that is not live, older than
  `logs_gzip_after_days`, becomes `.log.ndjson.gz` (a later run's output is
  appended as another gzip member). `view result`, `view watch` and every
  reader take either; `view result` falls back to the answer in the log when
  `result.md` is gone, and shows the `.gz` path.
- **Review refs**: `refs/relay/review/<job>` (the ref a review read its branch by) of
  a review that is gone, or ended `outputs_after_hours` ago and is not live. A review
  resumed later fetches it again.
- **Idle copies of the linked caches** ([`sandbox.linked_caches`](#the-sandbox)) in the
  pools (`farms/` in the data and state directories): one no run has taken for
  `cache_copies_idle_days`, and any past the `limits.max_running` used last in its pool,
  since no more runs than that hold one at once. Its files are the cache's own (hard
  links), so it frees directories, not data, and counts as freeing nothing.
- Never a clone with a live runner, never a symlink (skipped whole), never a
  path outside relay's own clone and state directories. A `build_outputs` entry
  must be a plain relative path (no `..`, not absolute).

The dispatcher runs a pass at most every `interval_minutes` while it is awake
(a stamp in the state directory keeps that across restarts), and at once when
free disk falls under `min_free_disk_gb` (not again within five minutes). A pass
stops cleanly at `pass_seconds` and the next goes on; only one runs at a time:
`relay land gc` while another pass runs does nothing and exits 4 (`gc: another
pass is running`), so try again once it is done.

**A ceiling for running jobs.** The runner measures a build job's clone when it
starts and every `disk_sample_minutes` (a recursive stat walk, bounded in time,
not following links), together with the job's scratch directory (`/tmp`), which a
read-only job has too. The size is on the board (`12G`) and in `--json`
(`disk_bytes`). When a clone passes `job_disk_gb`, or free disk falls under
`min_free_disk_gb` while it is the largest clone running, the `disk` [hook](#hooks)
point is reached. With no `disk` chain, or every handler deferring, the job is
stopped with the reason `disk: clone 52G over 40G` or `disk: 8G free, under 20G`,
its clone and session kept: free some space (`land gc` first), then `queue
resume`. A job stopped by the free-space floor is not left for you: see
[the floor holds](#the-floor-holds-a-job-and-resumes-it) below. `wait` lets it go on, and asks again only after an hour. While the
chain decides (an orchestrator can take `orchestrator_timeout_minutes`), the run
goes on under all its limits: its spend cap, silence limit and time limit still
end it, and a decision still waiting then is withdrawn (`decided` by
`orchestrator`, `defer`, `withdrawn: ...`) and refused if answered. A
`command:` or `agent:` handler still at work then is stopped with it (a command's
whole process group is killed, an agent's session ended), so the job ends, frees
its seat and `view result --wait` returns without waiting for the handler. Nothing is
ever deleted inside a running job's clone.

### The floor holds a job and resumes it

Free disk on a shared machine swings as other projects build, so the floor
(`min_free_disk_gb`) pauses a job rather than ending it. A job the floor stops
(`disk: 8G free, under 20G`) is **held**: it is `stopped` with `held` set (`held: true`
in `--json`), its clone, target and session kept, and the board shows
`held: disk: 8G free, under 20G; resumes at 23 GB free`. A clone over `job_disk_gb`
is not held, since free space does not cure it: that stop waits for a person.

The dispatcher looks at free disk every minute while a job is held. When it is at least
`min_free_disk_gb` plus `disk_resume_margin_gb` (default 3 GB, so a disk hovering at the
floor does not flap), it puts the held job that has waited longest back in the queue,
and its session is told the disk floor paused it, that nothing in its work caused it,
and that free space is back. Only one resumes at a time: the next waits until the first
has started and has run for one `disk_sample_minutes` (its clone measured), so the space
is not eaten again at once. That holds across the machine's projects (a job another
project resumed from a hold settles for this one too), and through an automatic retry of
the resumed job; a hand `queue resume` of it ends the wait, and each job settles for the
`disk_sample_minutes` of the project that resumed it. A held job whose `--after` jobs are
not done (a parent held after it) waits for them, and if one of them fails it is blocked
like a queued one (`waits for p (failed)`), so waiters on it exit 4 rather than wait forever;
`queue release NAME` frees it of them, and the dispatcher then resumes it when space allows.
A resume is another run, so it never passes `limits.job_ceiling_usd`: the run's cap is cut
to what the ceiling leaves, and a job whose ceiling is spent stays stopped with a reason
that says to raise it and `queue resume`. Turning the floor off
(`min_free_disk_gb = 0`) lets what is held resume, still one at a time. A held job not yet resumed counts as moving: a job queued
`--after` it waits for it, and `view result --wait` goes on waiting.

- A queued job obeys the same rule: a build job does not start while free disk is under
  floor plus margin (the board says `free disk below 23 GB (the 20 GB floor and its
  margin)`). Read-only jobs still start; the floor can still hold one that runs.
- The floor itself is unchanged: its value, and what stops a running job (free disk under
  it while that is the largest clone), are as above. The margin only moves when a job
  goes back.
- `queue stop` on a held job, or an earlier stop by a person, keeps it stopped for good:
  the dispatcher resumes only what the floor held. `queue resume` by hand still works
  at any time, and the dispatcher then leaves the job alone.
- A `disk` handler answering `stop` for a job under the floor holds it the same way.
  Nothing here deletes a held job's clone; `gc` keeps it by the rules above.

`[session] env` sets variables in every job's environment (and the dispatcher's), over
relay's own (see [What a job is](#what-a-job-is)) and the development environment's and under
the harness's own account variables; a `PATH` there replaces relay's default. Nothing is
taken from the shell that queued the job. relay hard-codes none: `CARGO_INCREMENTAL = "0"`
is the suggestion above.

A value may name the project's own directories without the home path: `${RELAY_STATE}` is this
project's state directory (an acceptance check's checkout is made under it) and `${RELAY_CLONES}`
its clones directory (every build job's clone). They are the only two names relay expands; any
other `${...}` stays as written.

**Recipe: mise.** Inside the sandbox `bun`, `node` and the like resolve to mise's shims
(`~/.local/share/mise/shims`), which read the `mise.toml` of the directory they run in and refuse
a clone's, or a check's checkout's, as untrusted (`mise ERROR Config files in …/mise.toml are not
trusted`, exit 1). relay knows nothing of mise; declare the trust yourself. mise trusts every
config below a path named in `MISE_TRUSTED_CONFIG_PATHS` (colon-separated):

```toml
[session.env]
MISE_TRUSTED_CONFIG_PATHS = "${RELAY_STATE}:${RELAY_CLONES}"
```

This trusts files written by the session's own clone, so use it only for a project whose
`mise.toml` you would run anyway (a `mise.toml` can set environment variables and run tasks).

## Harnesses and accounts

Every harness is scheduled alike. An adapter in [`src/harness/`](src/harness/)
says, for its CLI:

| | claude | omp | puku |
| --- | --- | --- | --- |
| an account | a `~/.claude*` dir with a refreshable sign-in | `~/.omp` (`default`) or `~/.omp/profiles/<name>` | a `~/.puku-cli*` dir whose sign-in (below) is refreshable |
| selected by | `CLAUDE_CONFIG_DIR` | `OMP_PROFILE` | `PUKU_CONFIG_DIR`, and `APP_NAME` for its sign-in |
| quota from | `claude -p /usage`, and each session's rate events | `omp usage`, per provider | puku's chat quota (`api-cli.puku.sh`), read by a node script |
| resume on another account | yes: the transcript is copied | yes: the session lives with the job, not the profile | yes: the transcript is copied |

An account is named `harness:label` (`claude:work`, `omp:default`).
`[harness.<id>]` in the config sets a harness's `model`, `effort`,
`extra_args` and `program`, or turns it off; `accounts.exclude` leaves out
single accounts.

A harness's program is `program` when set (a path, relative to the config file's directory
when it is not absolute, or a name), else its own name
(`claude`, `omp`, `puku-cli`). A name is the absolute path relay recorded the first time it found
it on a `PATH` (`$XDG_STATE_HOME/relay/programs.json`, kept while that file can be run;
changed only under `programs.lock`, so relays recording at once keep each other's entries),
so a shell whose `PATH` lacks it (a dispatcher woken from cron, a bare `env -i` shell)
still finds it, and a shell whose `PATH` names another does not replace it. Only an
executable file counts. A name found on a `PATH` is never a mise shim
(`~/.local/share/mise/shims/puku-cli`, a link to `mise`): a shim chooses its version from
the mise config of the directory it runs in, and fails (`No version is set for shim:
puku-cli`) where none pins it, which is where sessions run. The real binary an activated
mise put on `PATH` is found and recorded instead; a shim already in `programs.json` is
passed over and the record replaced by that real binary. A path the config names is the
owner's own choice, and is taken as it is. A harness is used when its program is found; one
the config has a `[harness.<id>]` table for whose program is not found is shown on the
board and in `quota show` as `program not found: omp` (`missing` in `--json`), and `queue
add --harness omp` exits 5 saying so, and how to provide one. To move relay to another
copy (an upgrade into a new directory), set `program`, or delete its line from
`programs.json` and run any relay command from a shell that has it.

A harness whose program is a script also has helper programs, the interpreters its
`#!/usr/bin/env` line names: `bun` for omp, `node` for puku. They are found and recorded the
same way, by the first command that finds the harness, and come first on the `PATH` of every
run of that harness (its sessions, quota readings and `relay probe`'s check), so a session never
runs whatever interpreter an earlier `PATH` directory holds. A helper is never a mise shim
(`~/.local/share/mise/shims/bun`, a link to `mise`): a shim chooses its version from the mise
config of the directory it runs in, and fails (`No version is set for shim: bun`) where none
pins it, which is where sessions run. Run the first relay command from a shell where mise has put
the real binary on `PATH` (an activated mise does), or delete the helper's line from
`programs.json` to find it again. A harness whose helper is not found is not used: one the
config names (`[harness.omp]`) is shown as `helper not found: bun`, and a job asked of it is
refused saying how to provide one (never by setting `program`, which names omp itself), one it does not name is left
out, as one whose program is not found is.

Who a claude directory is signed in as comes from its `.claude.json`
(`oauthAccount`), read locally, never by a call. relay shows only the plan and
the first 6 hex characters of a hash of the account id (`Ultra #76de9d`), and
stores and prints no email, name or raw id. A directory with no account is `not
signed in` and is never chosen. Two directories signed in as one account are
shown as `same sign-in as claude:first`; they share one quota (one reading,
the newest) and one seat budget, so a session on either counts against both.
omp keeps no identity relay can read, so it shows none.

puku-cli is a Claude Code derivative and runs through the same adapter: the same
flags (except `--god-mode` for `--dangerously-skip-permissions`), stream-json
events, session ids and transcripts. Its home is `~/.puku-cli` (`puku:default`) or
`~/.puku-cli.<label>`, but its sign-in is not in the home: `puku-cli auth login` writes
`~/.config/$APP_NAME/session.json`, with `APP_NAME` defaulting to `pukucode`. So relay
pairs `~/.puku-cli` with `~/.config/pukucode` and `~/.puku-cli.<label>` with
`~/.config/pukucode.<label>`, and sets `APP_NAME` to match; sign each labelled home in
with both variables set (Requirements). Who it is signed in as is a hash of that
file's user id (`#3f2a1c`: puku keeps no plan there). Its quota is the chat quota
puku's own Usage tab shows (`fiveHour`, `weekly` and `monthly` windows become the
meters `five_hour`, `seven_day` and `monthly`, in the 5 hours, 7 days and month columns); puku has no command that prints it, so
relay runs a short `node` script that sends the home's access token to
`https://api-cli.puku.sh/puku/v1/subscription` and prints only the quota. When the
token has expired, the script first runs `puku-cli auth status`, since every puku-cli
start renews an expired sign-in, so puku's file is written only by puku. A refused
token (401 or 403) or no sign-in marks the account signed out; a token that could not
be renewed leaves it unread. Both `puku-cli` and the script's `node` are found as a
harness's program is (above): `[harness.puku] program`, else the path recorded when relay
first found the name on a `PATH` (mise installs `puku-cli` under
`~/.local/share/mise/installs/npm-puku-puku-cli/`, with a shim in mise's `shims`). `node` is
a helper program of the puku harness: the first relay command that finds `puku-cli` finds and
records `node` too, from that caller's `PATH`, so the dispatcher and its quota reader, which run
on a narrower one, use the same file. That `node` also comes first on the `PATH` of every puku
run, whatever `session.env` names, since `puku-cli` starts through `#!/usr/bin/env node`: the quota
script, the `puku-cli` it renews through and every session all run the one interpreter. A `node`
not found is shown as `helper not found: node`, and puku is not used. In the same way every puku
run is told `HOME` (the user's home) and `APP_NAME`, which puku finds its sign-in by, so a job whose
`session.env` names another `HOME` still uses the sign-in relay found the account by.

Quota readings cost no model call. One is taken again after 10 minutes, or
once a reset it names has passed; a reading that fails is not retried for the
same 10 minutes. The dispatcher reads the accounts a queued job may use; showing
the accounts (`quota show`, `view status` with or without `--json`, `view watch`) starts
a reading of every stale one and says `refreshing` (`refreshing` in `--json`), so an idle machine's
board does not go stale. There is one reader on the machine: `relay quota refresh`
that finds a reading under way (any project's) waits for it, then reads only what
it did not. A reading relay asks for itself joins the project's pending set
(`<state>/quota.pending`), which one background reader per project reads to the
last, waiting its turn for the machine's reader, before it exits: an account shown
as `refreshing` is always read. The accounts due are read at the same time, one process each, under
that one hold of the machine's reader role, so a refresh of every account takes as long as the slowest.

A read ends at `accounts.read_secs` (default 180, at least 30: refused below it when the config
loads). `claude -p /usage` also computes "What's contributing to your usage?" from this machine's
local sessions, and `claude` has no flag or setting that skips it, so a heavy account is slow, slower
under load (58 s at load average 77). A read that gives nothing records why (`unreadable` carries
`why`): `timed out after 180 s`, `exited 1: <last line of stderr>` (cut, with anything shaped like an
email address or a token taken out; no raw output is kept), or `printed no meters`, with how many reads
in a row failed and since when. The board's state cell, `quota show --json` (`readFailure`) and a
queued job's reason say it instead of `stale` or `usage unreadable`: `reading timed out after 180 s
(3 tries since Nov 12 18:08 +06)`. Any good reading ends it, however it came (the quota command, a session's rate events, another project's or a twin home's reading); a failure after it starts a count of its own. The stderr tail has terminal controls replaced. An account is used only when:

A reset time is read against the meter's own window (5 hours, a week, a month): it counts when
it lies in the hour before the reading (the CLI keeps printing a reset that just passed for a
while, and it stays as printed: the meter is resetting now) and up to a window ahead. A time
outside that range, such as `resets 7pm` read at 8:30pm for a 5-hour meter, is no reset time
rather than a guess tomorrow or a year out. A stored reset further ahead than its meter's window
is read as no reset time too, so an older reading on the bus no longer shows `resets 365d`.

- it has a reading, and not a stale one (unread or stale is not the same as
  having room; a job waits while a fresh one is taken);
- the meters that govern the job's model are below `limits.floor` (omp meters
  each provider, so its model must name one: `minimax-code/MiniMax-M3.1-Flash`),
  or below `limits.floor_near_reset.floor` for a meter that resets soon (below). A
  meter of one model's own governs only a job whose model it covers, beside the
  account's (or provider's) whole ones. Which are declared, never read off a name:
  claude's model families (`Opus`, `Sonnet`, `Haiku`, `Fable`, in `Current week (Fable)`
  or `seven_day_opus`) cover a model whose id names them (`claude-fable-5-2`); any other
  week (`seven_day_overage_included`) is the whole account's. omp's antigravity groups
  are `Gemini` (models named `gemini`) and `Claude & GPT (shared)` (`claude` or `gpt`);
  a group not declared governs every model of its provider, and a model no meter covers
  is refused as blind. A claude job on the home's default model is judged by the whole
  ones alone, since relay cannot tell which model that is;
- it is not limited or signed out;
- it has a free seat.

A reading under way is waited for, briefly, rather than letting an account read
a moment ago take every job because the others are still being read (twelve jobs
queued at once once all went to the one account whose reading had landed). A job
that some account admits waits instead when another account it may use is refused
only for a stale or missing reading, that reading is due (no failed read is
backing off; the dispatcher has asked for it), and on its last reading that account
would rank above every one that admits the job now (a never-read one always might).
The wait lasts at most two minutes from when that reading went stale or the job was
queued, whichever is later, and is shown as what holds the job: `waiting for a
reading of claude:b before starting on claude:a (until 14:02 +06 at most)`. A job
that resumes its transcript on an account that admits it does not wait. Waiting was
chosen over starting on the last reading because waiting starts nothing: an
account is still only used on a fresh reading below its floor, so the wait can
delay a job but never drive an account past its floor. An old reading can say
there is room that has since been spent.

The accounts pane makes the same judgment, seats included, so an account it calls
`ready` is one the dispatcher would start a session on; a full one says
`busy (1 of 1 seat on openai-codex)`.

Quota that resets soon is lost if unused, so the floor can loosen near a reset:

```toml
[limits]
floor = 0.9
floor_near_reset = { within_hours = 12, floor = 0.98 }
```

For each meter whose `resetsAt` is within `within_hours` of now, the floor is
`floor_near_reset.floor` instead of `limits.floor`; every other meter keeps
`limits.floor` (it is per meter, not per account, and an unknown reset is never near).
The new floor must be above `limits.floor` and at most 1. Absent, nothing changes.
The accounts' gauges move that meter's `│` to it (`floorWhy` in `--json` says
`floor 98%, resets in 10h`), and a held reason names it too:
`at floor: 99% seven_day, resets Nov 4 13:15 +06 (floor 98%, resets in 10h)`.

Of those, relay picks the one with the most room left per day until reset,
because quota still unused at a reset is lost, shared among the sessions already
on its seat and the new one: an account with 30% a day and two sessions ranks as
10%, below an idle one with 20%. A pass counts each start it makes before it judges
the next job, and readings land minutes apart, so this is what spreads a burst:
twelve jobs on three accounts with like room start four on each, while an account
with twice the room per day (one resetting soon) takes about twice as many. A
session that already ran stays on its account while that account is usable,
since moving it loses the provider's prompt cache and its whole transcript is
read again at full price. The accounts' `room/day` column is the account's own
room, `run` the sessions on it, and `▸` marks the account the next start would
take (`next` in `--json`, `score` per account).

Accounts belong to the machine, not the project, so every project on it
shares what it knows of them: each listens to the others' state (never
writing it) and uses the latest reading, the later limit, and the latest
sign-in change (a sign-out stands until the account is enabled again in any
project), so a reading one project took is not taken again. Seats are shared
too: `limits.per_account` counts the sessions every project runs on the
account (for omp, on the account *and provider*: one profile serves several
providers, each with a quota of its own, so a `minimax-code/...` review does not
hold the seat an `openai-codex/...` job needs; two jobs on one provider still
share it). A decision's `agent:` session holds a seat too, from its `seated` to its
`unseated`. A seat is the account and provider the job's dispatch recorded
(`dispatched` carries the model and provider it resolved), so another project's
job that names no model counts on the provider its own config chose, not this
project's default. One machine-wide lock keeps two dispatchers from giving out the
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
checker = { harness = "claude", model = "claude-sonnet-5-5", budget_usd = 1.0 }
```

The check reads the brief, every earlier verdict, `git log --stat` since the
last checkpoint (with the diff when it is small) and the worker's last message,
and answers `continue`, `redirect` (with a correction the worker reads next) or
`stop`. On `continue` or `redirect` the same session resumes with the cap
times the growth; on `stop` the job is left `failed` for you. No new commit and
no changed file since the last checkpoint is a `stop` that asks no model, and so
is a clone that still stands exactly at the commit its acceptance check last
failed at (a run resumed with that failure that changed nothing): however much
the branch moved before that check, this run did not. A next cap that would take
the job past `job_ceiling_usd` always waits for you.
Read-only jobs, and any job without growth, stop at their cap as before.

A checkpoint that needs a checker waits for one. If no account can take the
check (every account is busy, at its floor or without a fresh reading), the job
stays in `checking` for up to `hooks.seat_wait_minutes` (default 30) and goes on
as soon as a seat frees. Only when that wait runs out does it stop, and its
checkpoint's `why` and the job's reason say so (`no claude account became free
for agent:checker within hooks.seat_wait_minutes = 30`).

A run that reaches its cap *after* it has ended with its FINAL section saying
`status: done` has nothing for a checkpoint to authorize: no more spend is
asked for. It ends `done` whether or not growth is on, with the overrun recorded
as an `overran` event, on the board (`over budget: $39.27 of $30.00`) and in
`view result`; its profile's acceptance check and the `ended` chain run as for
any other done run, so a chained `--review-of` job starts.
`relay view status` and `view result` show the latest checkpoint
(`checkpoint 2: continue → $30.00`); `--json` has all of them under
`checkpoints`. `relay queue resume NAME --budget X --message M` works from
`checking` or `failed` as ever. This is the `cap` [hook](#hooks) with its
default chain, `["rule", "agent:checker"]`; `hooks.cap` replaces the chain, and
with it set a cap is a checkpoint even without growth (the next cap is the
action's `budget`, else the same cap again).

## The sandbox

Each session runs under `bwrap`. It sees the machine read-only, except for:

- **Hidden** (shown empty, and read-only, so a write to one fails at once instead of
  filling RAM): `sandbox.hide`, by default your repository's
  siblings; every account of every harness but its own; relay's own state and
  other jobs' clones; the credential stores in `sandbox.hide_home`; and host
  sockets (`/run/user/<uid>`, sockets under `/run`), since a read-only mount
  does not stop a connection to a socket.
- **Writable**: its own clone and job directory, and its own account (for puku also
  its sign-in directory under `~/.config`, which a session rewrites when it renews
  the token), except what in it the host later runs or obeys (claude's and puku's
  settings, hooks, plugins, skills and scripts; omp's config, agents, extensions and natives, and the
  default profile's `profiles`, where new accounts would appear), which is
  mounted back read-only. One the account does not have yet is protected too: relay
  makes it empty first (an empty directory or file, whichever the adapter says it is; `{}` for a `.json`) and
  mounts that read-only, so the session cannot be the one to create it. These
  empty stand-ins stay in the account afterwards, and mean what their absence did.
- **Read-only: the job's inbox** (`<job>.in` in the jobs directory), where relay
  writes what it hands the session, such as omp's prompt file. relay never writes
  in a directory a session can write, since a symlink planted there would take the
  write wherever it points: its own files go in an inbox, opened once without
  following a symlink and written through that handle, each file created afresh.
  A decision's agent session gets a directory and an inbox of its own
  (`<job>.check.d`, `<job>.check.in`), and the acceptance check a directory made
  fresh for each run (`<job>.bar.d`), never one the worker could have written.
- **`/tmp`**: a directory on disk, never RAM. Each job gets its own scratch
  directory, `<data dir>/<project>/scratch/<job>`, bound at `/tmp` (`TMPDIR` stays
  `/tmp`), created when the job starts. A read-only job (a review) cannot write its
  repository, so this is where it builds, with `CARGO_TARGET_DIR=/tmp/...` for one.
  It counts in the job's disk use (`limits.job_disk_gb`, the board's size column) and
  gc removes it with the clone's build outputs ([disk](#disk)). Hidden directories
  are empty and read-only RAM tmpfs, so they hold no job's data (the rest of `/dev` is
  bubblewrap's own small device tmpfs).
- **`/dev/shm`**: a RAM tmpfs capped at `sandbox.shm_mb` MiB (default 256), for what
  wants one: POSIX shared memory, or a test that keeps fsync cheap. A write past the cap
  fails with ENOSPC instead of filling RAM; `shm_mb = 0` makes it empty and read-only.
- **Caches** (`sandbox.caches`, default `~/.cache` and cargo's registry and
  git checkouts): writable, but relay's own directory for the project, shared
  by its sessions and never the host's. A session that could write the host's
  `~/.cargo` could replace what the host runs. The first build fetches afresh.
  A cache whose path the host does not have is left out, as before.
- **Linked caches** (`sandbox.linked_caches`, default none): the caches a tool hard-links from
  into the tree it installs. `link()` fails across two mounts (EXDEV), even on one filesystem, so
  with a cache bound apart from the tree such a tool copies every package instead (Tickify: a frozen
  `bun install` of 4 s on the host did not finish in 1800 s in a check; bun's own copy backend did not
  either). **Declare a cache here when a tool links from it**: bun (`.bun/install/cache`), pnpm (its
  store), uv (its cache, which it links from by default on Linux). Each must also be in
  `sandbox.caches`; relay refuses the config otherwise. Do not declare one nothing links from (cargo
  only reads its registry): it would cost every run and save nothing. A project that declares none
  has every cache a bind, exactly as before.

  A job with a tree (a session's clone, each run of its acceptance check) gets each linked cache as a
  **symlink** into its own copy of it, inside the mount it works in (`.git/relay-caches` in the
  clone; `caches/` in `<job>.bar.d`), so `link()` from the cache into the tree works:

  | what | where the job sees it | what it is |
  | --- | --- | --- |
  | a linked cache (`~/.bun/install/cache`) | its path under the home | a symlink into the job's copy |
  | the job's copy of the linked caches | in its tree's own mount | a directory per directory of relay's cache and a hard link per file; no file's data |
  | the directory holding the cache path (`~/.bun/install`) | where it is | the host's entries, each bound back read-only (a symlink as itself), on an empty tmpfs made read-only: what showed before shows, what is hidden stays hidden, and nothing can be created there; entries the host makes later do not show |
  | every other cache (`~/.cache`, …) | its path under the home | a bind of relay's directory, as before |
  | relay's caches themselves | nowhere | hidden, with the rest of relay's own directories |

  **What it costs.** The copies are kept, idle, in a pool beside where they are used (`farms/` in
  the project's data directory for sessions, in its state directory for checks), so the link per
  file is made once per copy, not per run. A run takes one by renaming it into place, brings it up
  to date when it starts, links back into relay's cache every name it added when it ends, and renames
  it back. Each of those two passes reads every directory of the linked caches on both sides, and
  touches only the files one side lacks, so **every run pays two passes over the cache's
  directories**, and the first run on a new copy also pays a link per file. Measured on this
  machine's overlayfs under heavy load (load average ~70 on 12 CPUs): a 105 MB bun cache of 10,931
  entries in 627 directories cost 236 s to copy the first time, then 24 s at the start and 27 s at the
  end of each run; a 113 MB cargo registry (1,338 directories) cost 64 s and 37 to 87 s. On an idle
  disk these are a fraction of a second to a few seconds. Each copy holds one inode per directory of
  the cache; a link takes none.

  **Shared and persistent.** What one job adds is there for the next. Two jobs adding one entry at
  once each write their own, and the first linked back is kept; an entry already there stays as it
  is on each side, so a file a job replaced by renaming over it stays in its copy only, while one
  changed in place is the shared file itself (one inode), as when the cache was one bind. With hard
  links working, a tool that edits an installed file in place (patch-package) changes the cache for
  every job, as with bun or pnpm on any machine. A directory a job added appears in the shared cache
  whole (built beside the caches, then renamed into place), so a run taking its copy meanwhile never
  gets half a package. Relay walks a copy only after its job's sandbox has ended, and walks both sides
  from an open handle one directory at a time, never following a symlink in either. A clone's measured size
  ([disk](#disk)) leaves its copy out, since its files are relay's cache's. gc removes an idle copy
  unused for `gc.cache_copies_idle_days` (default 1), and any past the `limits.max_running` used last
  in its pool. `relay probe` checks the layout from inside for a project that declares a linked cache.
  Without the sandbox, for a job with no tree (a review, a decision's agent), where the copy cannot
  be made (a test link from the cache into the copy fails: another filesystem, or another mount of
  the same one), where something else is mounted at or under the cache path (a `hide_home` path
  inside it), or where another cache is nested in it or around it (`.cache` and `.cache/uv`), a
  linked cache is a bind as the others, the tool copies, and no copy is made: a cache relay leaves a
  bind is never in a job's tree, and a copy from the pool is cut down to what this run links. A cache
  nested in another (`.cache` and `.cache/uv`) is refused when the config is loaded: a session could
  reach the inner one's directory through the outer one's bind and plant a symlink on relay's way to it.

  **What holds when a run fails or is interrupted.** Four invariants, for every path below: (1)
  nothing a hide rule covers becomes readable through relay; (2) relay writes nothing outside its
  caches and the job's own copy, and links nothing from outside them; (3) no half-written directory is
  published into a shared cache; (4) nothing relay leaves behind stays for ever. Tests are in
  `src/sandbox/farm.rs`, `src/sandbox.rs`, `src/systems/probe.rs`, `src/systems/land/gc.rs` and
  `tests/flow.rs`.

  | path | what can go wrong | why it holds |
  | --- | --- | --- |
  | which caches are copied | a cache left a bind (a hide rule under it, a later layer over it) copied anyway | one judge, `sandbox::linkable`, decides both the plan and what the copy is taken for (1; test: a cache with a hidden path under it is never copied) |
  | config change between runs | a pooled or left-behind copy holds a cache this run does not link | a copy is cut down to this run's caches when taken (1; test: a pooled copy holds only the caches it is taken for); with none to take, one left in the clone is removed, and the job does not start if it cannot be (1; test: a copy left in place goes when this run links nothing) |
  | resume after a crash | the runner died with the copy in the clone | the next run takes it in place, cut down, or removes it as above; a check's directory is made fresh for each run, its copy with it (1, 4) |
  | take | a directory on the way to a cache swapped for a symlink | every shared walk starts from an open handle of relay's caches and opens one directory at a time without following a link; anything else on the way is refused, never replaced (2; test: the way to a shared cache follows no symlink) |
  | take | the copy on another filesystem, or on another mount of the same one | a real link (an unnamed `O_TMPFILE`, or a named probe linked into the cache itself) before anything is walked; on failure the cache is a bind (tests: a copy on another filesystem, on another mount of one filesystem, and without `O_TMPFILE`) |
  | take fails halfway | a partly made copy stays in the job's tree | it is removed through its parent's handle, and the job does not start if it is still there (1) |
  | release | the job moved or swapped the directories around its copy | the copy is walked through the handles opened when it was taken, after the sandbox's PID namespace has ended (2; test: a release follows nothing the job left) |
  | release, a new directory | a run taking its copy meanwhile sees half of it | built in `.relay-staging` beside the caches, renamed into place with no replace (3; test: a take during a release sees each new package whole or not at all) |
  | release, an entry cannot be staged | a partial package published | a directory with any entry left out of its staging is not published (3; test: a new directory that could not be staged whole is not published) |
  | release interrupted | a directory left half-built in staging | staging is outside every cache; a release sweeps the builds no release holds (3, 4; test: what an interrupted run left is swept) |
  | a release that takes long | its build swept while it still builds, and files of a package it just published removed | each build holds a lock file (`flock`) from before its directory is made until after it is gone, and checks the lock is still the one named there; the sweep removes only a build whose lock it can take, or that has none, by its name in staging, never through a handle kept across a publish; age is never evidence (3; test: a build held by its lock is never swept however old) |
  | probe of the link, interrupted | its file left in a cache | its name is skipped by every walk and swept after an hour (4; same test) |
  | gc prune | a run takes a copy as gc removes it | taking and removing both begin with a rename, so one of them gets it; a copy whose removal failed (`gone-*`) is removed again by the next pass (4; tests: idle cache copies go by age and past the most runs at once; an idle copy holding an unreadable directory is removed) |
  | relay probe | a marker written through a planted link, or into a cache the plan leaves out | markers go only in the caches the plan mounts, through handles that follow no link, and are removed on failure (2; test: markers go only in mounted caches and through no symlink) |
  | a cache's bind | a symlink on the way to its directory in relay's caches | caches are never nested, so nothing on the way is in any sandbox's bind, and the cache directory itself is a mount point in any sandbox that binds it; it is made through handles that follow no link (2) |
  | what a job could read before | it renamed its old copy inside its clone, out of relay's reach | it keeps only what it could read when it did (it could equally have copied it); relay exposes nothing new (1) |
  | a cache's host path is a symlink | `~/.cargo/git -> .private` with `hide_home = [".private/secret"]`: the hide mount lands on the host's directory, not on the copy | every rule compares paths as written, so a cache whose path, or a directory on its way under the home, is a symlink is never linked; as a bind, the hide mount lands inside the cache's bind and masks the subtree (1; test: a cache whose host path is a symlink is not linked) |
  | relay's own files in a cache | `relay probe`'s marker copied into a job's copy and written back into the shared cache | one predicate names relay's own files (link probes, probe markers), and every walk, either way, skips what it names; the probe puts its marker in its own copy (2, 4; test: relay's own files are linked by no walk) |
  | a cache's spelling | `./.cache` and `.cache/x` taken for two caches apart, or `./.relay-staging` for a cache | a cache path is one type, made once when the config is read, of plain names only; the checks, the plan, the copies and the probe compare only that (2; test: the bad-config list) |
  | which pairs are linked across | staging on another mount than the copy, so every release fails | before a copy is used, a real link is made from each cache and from the staging directory into it: each is one mount with the copy, so all are one (tests: on another mount of one filesystem; when staging cannot be linked into) |
  | filling a copy, building a new directory | a link that fails (EMLINK, ENOSPC, an unreadable directory) passed over, a package half there | the walk is all or nothing for every caller: any failure is an error; a copy is then removed and the cache is a bind, a build is removed and nothing is published (3; tests: a copy that could not be filled whole is not taken; a new directory that could not be staged whole is not published) |
  | relay probe, any way out | a marker left in relay's caches, or in a copy given back to the pool | the markers are owned by a guard whose drop takes them out of the copy, gives the copy back, then takes them out of relay's caches; a killed probe's markers go at the next probe (4; tests: markers go when what seeded them is dropped; markers leave the copy before it is pooled) |

What is left open, by design or by necessity:

- **The network**, since the session needs its provider's API, and with it
  abstract Unix sockets.
- **claude's `.claude.json`** (and puku's `puku-cli.json`), which can name MCP
  servers the host would start later. claude rewrites it by renaming a new file over it, which a read-only
  mount would break.

The mounts are one declarative plan in layers ([`src/sandbox.rs`](src/sandbox.rs)),
where the job's own mounts come last, so nothing above can hide them.

Environment variables are cleared and relay's own (built from the config, never the
calling shell's) are passed in, except those matching
`sandbox.env_deny` (tokens, secrets, keys, `AUTH`, cookies, sessions and
`CLAUDE*` and `PUKU*`). The adapter then sets the one that selects the account. The
same list applies to `[session.env]`: an entry it names (`PUKU_WORKER_URL`, `NPM_TOKEN`) is
refused when the config is loaded, naming the variable and the pattern, rather than dropped,
so narrow `sandbox.env_deny` if a job really needs one. A
project whose toolchain comes from a development environment that cannot
start inside the sandbox (single-user Nix locks its store, which is
read-only there) names it in `session.env_shell`, such as
`["nix", "develop", "-c"]`: relay enters it on the host, as direnv would, passes
its variables in through the same filter, and tells the session its tools are
already loaded ([the development environment](#the-development-environment)). Signing
keys are hidden, so git inside is told not to sign (`commit.gpgsign` and
`tag.gpgsign` off); sign commits when you land them.

### The development environment

Entering `session.env_shell` belongs to the project, not to a job: on a cold Nix store it can
take hours, and sessions that each entered it would race to build the same shell and fail
at their time limit. So relay enters it **once**, and every session and acceptance check reads
what was recorded:

- **One warm-up.** When a queued job is free to start and the environment is not recorded
  for the current key, the dispatcher starts `relay devshell warm`. It enters the shell in your
  repository (never a clone a session could have edited) and records its variables, after
  `sandbox.env_deny`, in `devshell.json` in the project's state directory, which only you can read.
  It holds a lock (`devshell.lock`), so at most one runs per project however many jobs wait.
  The shell runs supervised and holds the lock too (as its stdin), so a warm-up killed midway
  leaves none running: its shell is ended with it, and no new one starts until all of it has;
  `relay devshell warm` by hand does the same, now, and exits 4 while another runs.
- **The key** is the shell command, `[session] env` (what the shell is entered from),
  `sandbox.env_deny` (what is left out of the record) and the repository's files that define a shell:
  `flake.nix`, `flake.lock`, `shell.nix` and `default.nix` (Nix); `devenv.nix`, `devenv.lock`,
  `devenv.yaml` and `devenv.local.nix` (devenv); `devbox.json` and `devbox.lock` (devbox); and
  `mise.toml`, `mise.local.toml`, `.mise.toml`, `.mise.local.toml`, `.config/mise.toml`,
  `.config/mise/config.toml`, `.mise/config.toml` and `.tool-versions` (mise). When any of them
  changes, the record no longer counts and the next job waits for a new warm-up.
- **Still whole.** The record also lists the paths the environment stands on, of those that existed
  when it was taken: every `PATH` directory, and every store path (`/nix/store/<name>`,
  `/gnu/store/<name>`) any of its variables names. Before the record admits a job, or `relay devshell
  warm` calls it `warm`, each is looked at (a stat); while one is gone, as after `nix-store --gc`
  collected it, the board says `dev shell needs warming again: /nix/store/…-cargo-1.99 is gone` and a
  warm-up enters it again, under the same key.
- **Held, not failed.** Until the record holds the current key, every job (read-only ones too,
  since a brief may run the shell's tools) stays queued with the reason on the board:
  `dev shell warming (12m)`, or `dev shell not warmed yet` for the moment before one starts. A
  job started just before the key changed goes back to the queue and waits with the rest.
- **A failed warm-up** is on the board with why and the last line of its stderr, `dev shell
  failed: exited 1 (attempt 2, retry 14:05 +06): error: unable to download ...`; the whole
  stderr of the latest attempt is in `devshell.stderr` beside the record, its end in `view status
  --json` (`devshell`) and on the bus (`warm-failed`). It is tried again after a minute, then
  after twice as long each time, up to 15 minutes; fix the cause and run `relay devshell warm`
  to try at once. The jobs wait meanwhile and are never failed for it.
- **The attempt limit** bounds one attempt, not a job. It is 600 s unless `[session]
  env_shell_attempt_secs` says otherwise (a value under 30 is refused when the config is loaded). A
  warm-up that takes longer is stopped there and tried again at once, and what Nix fetched stays in
  its store, so each attempt goes further; the board says `dev shell warming (2h); attempt 13 running;
  the last timed out after 600 s` (the limit that attempt ran under), so slow (attempts that keep timing out, and the warming time keeps
  growing) is told from stuck (one that fails, with its reason, as above), and the jobs stay held
  until one attempt finishes. A large flake on a loaded machine may need more than 600 s for one
  step; raise the limit rather than turning `env_shell` off:

  ```toml
  [session]
  env_shell = ["nix", "develop", "-c"]
  env_shell_attempt_secs = 1800
  ```

  One step that cannot finish in any limit (a single download that slow) never does: enter the shell
  by hand once (`nix develop -c true`), and the next attempt is quick.

**Keeping it through a garbage collection.** The record is a list of variables, not a Nix GC root,
so `nix-store --gc` may collect what it names, and the next job then waits for a warm-up that
fetches or builds it again. To keep it, give `nix develop` a profile, which is a GC root:

```toml
[session]
env_shell = ["nix", "develop", "--profile", "/home/me/.local/state/nix/devshells/myproject", "-c"]
```

relay does not add it itself: `session.env_shell` can be any command (`devenv shell`, `devbox run`,
`mise exec --`), and only `nix develop` takes `--profile`; a root it made would pin store paths you
did not ask it to, and a GC is how you free them. The check above keeps a collected environment from
being used either way; the root only saves the warm-up after it.

A job started on a recorded environment keeps it for its whole run: each attempt reads the
record once, when it starts, and its session and both runs of its acceptance check (at the tip and
at the base) get that same environment. A warm-up for a new key changes only the attempts that
start after it.

The session also gets its own PID namespace, so nothing it starts outlives it.
With the sandbox as the boundary, the session runs without approval prompts. Without it
(`sandbox.enabled = false`), read-only jobs use the harness's read-only mode
and build jobs its edits-only mode.

`relay probe` checks the plan from the inside, for one account of each
harness: every hidden path is empty and no socket, with a control proving it
exists outside, and no hidden directory is writable; `/dev/shm` is writable within its cap
(or not at all at `shm_mb = 0`); every writable one is writable
and every read-only one (placeholders and the inbox included) is not; `/tmp` is relay's directory on disk;
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
  backstop, and exits after a minute with nothing to do: nothing queued that can
  start, and no job held by a runner, running or checking.
- **Single roles:** the dispatcher and the development environment's warm-up (one each
  per project) and the quota reader (one per machine, since accounts are the machine's)
  each hold a `flock`, which the kernel
  releases if the holder dies.

| System | Responsibility | Emits |
| --- | --- | --- |
| queue | takes work in (under a profile, if asked), withdraws it, stops it, releases it from its `--after` jobs, reroutes or forces a held one, replaces or detaches a review chain | queued, released, resumed, rerouted, forced, dequeued, unchained, review-briefed, stop-requested |
| dispatch | gives queued jobs a seat on an account; reaps dead runners | dispatched, reaped |
| session | runs one job, carries out the decisions at its cap and its end, runs its acceptance check, fetches a review's branch, measures its clone and stops it at the disk ceiling, and reports what happened | cloned, started, rate, limited, signed-out, follow-up, requeued, overran, checking, checked, bar-checked, reviewed, disk, ended |
| decide | runs a decision point's handler chain, in the process that reached it (a runner, or `decide held`, which dispatch starts); books an `agent:` session's seat; takes the orchestrator's answers | decision-requested, seated, unseated, parked, decided |
| quota | account readings and switches | reading, unreadable, signed-out, account-enabled |
| land | brings branches back; cleans up, and gc | forgotten, collected |
| devshell | enters the project's development environment once, under a lock, and records it for every session | warming, warmed, warm-failed |
| view, probe | read-only (`view stats` folds the bus; it writes nothing). Showing the accounts may start a quota reading, which quota writes | — |

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
its dependents (`relay queue release` starts them anyway) instead of freeing them.

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
