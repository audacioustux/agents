---
name: relay
description: 'Use when an agent needs another agent harness (claude, omp) to do work: `relay ask` for a one-shot read-only answer now, under a safety contract (read-only, resume forks, no --continue, prompt never in argv); `relay queue add` for unattended work that runs sandboxed on whichever account of any harness has quota to spare. Not for calling a CLI without the contract (call it directly), and not for reading or ranking session stores.'
---

# relay

One tool, two ways to hand work to another harness. Both go through the same
per-harness adapters (`relay/src/harness/`), so each CLI's flags are
described once.

| | `relay ask` | `relay queue add` |
| --- | --- | --- |
| For | a second opinion, a plan review, a diff review | a task to run unattended |
| Runs | now, in your environment and account | when an account has room, in bubblewrap |
| Writes | nothing (read-only contract) | its own clone and branch `relay/<name>` |
| Needs | the harness CLI | a `relay.toml` ([relay/README.md](../../../relay/README.md)) |

Install once: `cargo install --path relay --locked`.

## relay ask

```sh
relay ask claude ask "is this lock order safe?"
relay ask omp adversarial docs/plans/migration.md
relay ask claude review --base main --head HEAD
relay ask claude plan docs/plan.md --dry-run        # prints the command as JSON, prompt redacted
```

`MODE` only shapes the prompt: `ask` passes the subject through, `plan` and
`adversarial` frame a review of it, `review` sends `git diff BASE..HEAD`
(default `HEAD~1..HEAD`). Flags: `--resume ID` / `--fresh`, `--model M`,
`--extra TEXT`, `--cwd DIR`, `--dry-run`. Unknown flags are errors; text after
a bare `--` is subject.

### The contract

Checked before the CLI starts; a harness that cannot honour a rule is refused,
never approximated:

- **Read-only.** A removed capability on `claude` (plan mode).
  On `omp` it is approval-gated (`--approval-mode always-ask`): the model can
  still write if someone approves. For unattended review of untrusted input,
  use claude.
- **A resume forks.** `omp` cannot fork, so `--resume` is refused there.
- **No `--continue` / `-c`.** They pick the latest session globally, from any
  repository. Use `--resume ID`.
- **The prompt is never in argv** (`/proc/<pid>/cmdline` is public, and argv
  has a size limit): stdin for claude, a prompt file for omp.
- Subject files are read up to 20 KB and diffs up to 1 MB, truncation marked.

Treat the answer as a hint and verify it. For multi-turn or
plan-then-implement work, use a [worktree](../using-git-worktrees/SKILL.md)
or `relay queue`.

## Driving it

Every command takes `--json` (one JSON value on stdout) and exits with its
kind of failure: 2 usage, 3 not found, 4 wrong state, 5 refused by a rule, 1
anything else. Branch on the code; read `error` only to explain it.

## relay queue

```sh
relay queue add fix-parser --prompt briefs/fix-parser.md
relay queue add audit --text "Review src/auth for races." --readonly --harness omp
relay view status          # jobs, and every account's room
relay view result fix-parser --wait --json   # blocks until it ends: no polling
relay land fetch fix-parser
```

A job that reaches its spend cap stops (`failed`) unless `limits.budget_growth`
is set: then it is `checking` while a read-only progress check on another
account judges the work since the last checkpoint, and it resumes with a larger
cap on `continue` or `redirect`, or waits for you on `stop`. No new commit
means `stop` with no model asked; `limits.job_ceiling_usd` bounds the total.
`relay queue resume NAME --message M --budget X` still works; `--budget` caps
that run alone.

The brief is the session's only context. See
[relay/README.md](../../../relay/README.md) for accounts, quota, the sandbox
and moving sessions between accounts.

## Adding a harness

One file in `relay/src/harness/` implementing `Harness`, added to `ALL`. It
says how to select an account (an env var or a profile flag), how each
`Task` field becomes flags, how to read its output and its quota, and what it
lacks (`Caps`). Add its flags to the tests that check them against the
installed CLI (`tests/ask.rs`): CLIs rename flags between versions, so rerun
`cargo test` after upgrading one.
