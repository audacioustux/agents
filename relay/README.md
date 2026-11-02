# relay

Queue coding-agent tasks for any git repository and let them run unattended:
each task is a headless session of an agent harness (Claude Code, omp,
puku-cli) inside a bubblewrap sandbox, on whichever account of whichever
harness has the most quota to spare. When an account runs out mid-task, the
session moves on: to another account of the same harness with its transcript,
or afresh to another harness.

`relay ask` is the other half: a one-shot, read-only call to any harness, now,
in your own account ([the skill](../.agents/skills/relay/SKILL.md)).

```sh
relay queue add fix-parser --prompt briefs/fix-parser.md --model claude=sonnet
relay view status                 # who is working on what, and each account's room
relay view result fix-parser      # the session's final report, branch and cost
relay land fetch fix-parser       # its branch, copied into your repository
```

Linux only. It replaces `lanes`, the Deno runner that was here before. See
[Coming from lanes](#coming-from-lanes).

## Requirements

- Linux, [bubblewrap](https://github.com/containers/bubblewrap) (`bwrap`), git, and `nice` and `timeout` from
  coreutils.
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

The session is told to end with a `FINAL` section. If it ends without one, it
is asked once more, since ending the turn ends the job.
[`rules.example.md`](rules.example.md) is a template of general rules for
unattended sessions, each with its reason. Edit it for your project and
prepend it to your briefs, or have each brief point to it.

| Command | |
| --- | --- |
| `relay queue add NAME (--prompt FILE \| --text TEXT) [--readonly] [--harness H,...] [--model H=M] [--effort E] [--budget USD] [--account H:L] [--task TITLE]` | Queue a job. `--harness` limits where it may run; `--account` pins it, and it waits for that account |
| `relay queue resume NAME --message TEXT [--budget USD]` | Continue an ended session, on any account with room |
| `relay queue dequeue NAME` / `relay queue stop NAME` | Withdraw a queued job / end a running one |
| `relay view status [--all] [--json]` | Jobs and accounts. `--json` prints the whole folded state |
| `relay view result NAME` | Final report, status, cost, branch, log path |
| `relay view events` | The merged event log |
| `relay quota show` / `refresh` / `enable L` / `disable L` | Account room, a fresh reading, switches |
| `relay land fetch NAME` / `land clean NAME` | Copy the job's branch into your repository (no checkout) / delete its clone and records |
| `relay probe [--account H:L]` | Sandbox self-test for one account of each harness, with controls |
| `relay ask AGENT MODE ...` | One-shot read-only call, outside the queue ([the skill](../.agents/skills/relay/SKILL.md)) |

Nothing needs starting by hand. Every command that changes something wakes the
dispatcher, which exits on its own a minute after the queue empties.

## Harnesses and accounts

Every harness is scheduled alike. An adapter in [`src/harness/`](src/harness/)
says, for its CLI:

| | claude | omp | puku-cli |
| --- | --- | --- | --- |
| an account | a `~/.claude*` dir with a refreshable sign-in | `~/.omp` (`default`) or `~/.omp/profiles/<name>` | `~/.puku-cli` |
| selected by | `CLAUDE_CONFIG_DIR` | `OMP_PROFILE` | its home |
| quota from | `claude -p /usage`, and each session's rate events | `omp usage`, per provider | not metered |
| resume across accounts | yes, the transcript is copied | no; it resumes on its own account | no |

An account is named `harness:label` (`claude:work`, `omp:default`). Two claude
directories signed in as the same person are one account, since they share one
quota. `[harness.<id>]` in the config sets a harness's `model`, `effort`,
`extra_args` and `program`, or turns it off; `accounts.exclude` leaves out
single accounts. puku-cli's adapter is not yet checked against the real CLI,
so it runs only when enabled explicitly.

Quota readings cost no model call. One is taken again after 10 minutes, or
once a reset it names has passed; a reading that fails is not retried for the
same 10 minutes. An account is used only when:

- it has been read (unread is not the same as having room);
- the meters that govern the job's model are below `limits.floor` (omp meters
  each provider, so its model must name one: `minimax-code/MiniMax-M3.1-Flash`);
- it is not limited or signed out;
- it has a free slot.

Of those, relay picks the one with the most room left per day until reset,
because quota still unused at a reset is lost.

When a session hits a limit, its account is closed until two minutes after the
stated reset (or for 15 minutes if none is stated), and the job goes back in
the queue. A session whose sign-in has expired marks its account signed out
until you sign it in again and run `relay quota enable H:L`. A session that
dies with no output is retried `limits.max_retries` times. A session that
reaches its spend cap is never resumed automatically, because spending more is
your call. Budget, wall clock and silence limits are enforced by relay, the
same for every harness.

## The sandbox

Each session runs under `bwrap`. It sees the machine read-only, except for:

- **Hidden** (shown empty): `sandbox.hide`, by default your repository's
  siblings; every account of every harness but its own; relay's own state and
  other jobs' clones; and the credential stores in `sandbox.hide_home`.
- **Writable**: its own clone and job directory, its own account, and the
  caches in `sandbox.writable_home`.

The mounts are one declarative plan in layers ([`src/sandbox.rs`](src/sandbox.rs)),
where the job's own mounts come last, so nothing above can hide them.

Environment variables are cleared and passed back in, except those matching
`sandbox.env_deny` (tokens, secrets, keys, `AUTH`, cookies, sessions and
`CLAUDE*`). The adapter then sets the one that selects the account.

The session also gets its own PID namespace, so nothing it starts outlives it.
The network is shared, since the session needs the API. With the sandbox as
the boundary, the session runs without approval prompts. Without it
(`sandbox.enabled = false`), read-only jobs use the harness's read-only mode
and build jobs its edits-only mode.

`relay probe` checks the plan from the inside, for one account of each
harness: every hidden path is empty, with a control proving it exists outside,
and every writable one is writable.

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
  system's event types. The types enforce this: a system can only build a
  port for its own event enum. A line that is not its file's type is dropped
  on read.
- **State:** everything is derived by folding the bus. Nothing else is stored.
- **Wake-ups:** the files are the lossless record, and an inotify watch is
  only a doorbell.
- **Single roles:** the dispatcher and the quota reader each hold a `flock`,
  which the kernel releases if the holder dies.

| System | Responsibility | Emits |
| --- | --- | --- |
| queue | takes work in, withdraws it, stops it | queued, resumed, dequeued, stop-requested |
| dispatch | gives queued jobs a slot; reaps dead runners | dispatched, reaped |
| session | runs one job and reports what happened | cloned, started, rate, limited, signed-out, follow-up, ended |
| quota | account readings and switches | reading, unreadable, account-enabled |
| land | brings branches back; cleans up | forgotten |
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
- **Scheduling extras:** `--after` dependencies and per-model caps.

Like lanes, relay never pushes or merges. Landing work on your main branch
stays with your project's gate.

## Development

```sh
cargo test                          # unit tests, plus end-to-end flows with stub claude and omp in bwrap
cargo clippy --all-targets          # pedantic lints are on
RELAY_TEST_KEEP=1 cargo test --test flow   # keep the flow's files for a post-mortem
```
