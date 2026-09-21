---
name: harness-bridge
description: Use when an agent needs to invoke a sibling agent CLI — currently `claude`, `puku-cli`, or `omp` — under a small safety contract: `--continue/-c` is rejected, `--permission-mode plan` (or its equivalent) is forced, `--fork-session` is forced on resume where the CLI supports it. The bridge is general-purpose (any task the caller would otherwise pass to a CLI), not limited to "second opinion" framing. Adding a new CLI is one entry in the `AGENTS` map if it accepts the same argv shape, or a small per-CLI argv hook if it does not. Not for invoking a CLI without the safety contract (call the CLI directly); not for parsing or scoring session JSONL (the bridge does not read session stores).
---

# harness-bridge

Bridge from one agent to a sibling agent CLI under a tight safety contract.
The wrapper is a small Deno script. Its only job is to make a few unsafe
invocations *physically impossible* — see [Safety contract](#safety-contract).
The bridge is **general-purpose**: it does not assume the caller's task is a
"review" or "second opinion." Review-flavoured prompts are one of several
modes, not the frame.

## Run it

```bash
deno task run -- claude ask "challenge this approach"
deno task run -- puku-cli adversarial docs/plans/migration.md
deno task run -- omp ask "challenge this approach"
deno task run -- claude review --base main --head HEAD
deno task run -- puku-cli ask "is this safe?" --fresh --dry-run
```

Or call the entry point directly:

```bash
deno run -A src/main.ts claude ask "challenge this approach"
```

The `deno task run --` form passes everything after `--` as the script's argv
(the `--` itself is consumed by `deno`).

The mode only shapes the **prompt** the sibling CLI receives. The contract is
the same in every mode.

| Mode | Prompt shape sent to the sibling CLI |
|---|---|
| `ask` | Pass the question / file / description through, prefixed with reviewer rules |
| `plan` | "Review this plan for correctness, missing steps, sequencing risk, …" |
| `adversarial` | "Attack assumptions, hidden coupling, failure modes, …" |
| `review` | Run `git diff <base>..<head>` and ask for an adversarial code review |

Useful flags:

| Flag | Purpose |
|---|---|
| `--resume <id>` | Resume a known session; `--fork-session` is forced |
| `--fresh` | Explicitly start a new thread |
| `--dry-run` | Print redacted argv; do not invoke the child CLI |
| `--model <name>` | Override model (honoured by all three CLIs) |
| `--base <ref>` / `--head <ref>` | Diff range for `review` mode (default `HEAD~1`..`HEAD`) |
| `--extra "..."` | Additional instructions appended to the prompt |

## Safety contract

The wrapper rejects or forces the following at parse time:

- `--continue` and `-c` are **rejected**. They resume the most recent session
  globally, ignoring the current repo. Use `--resume <id>`.
- `--permission-mode plan` is **always added** for `claude` and `puku-cli`;
  `omp` is forced into `--approval-mode always-ask` instead. Each CLI's
  read-only mode is pinned in its `AGENTS` entry — without it the model
  can write to the repo.
- `--resume <id>` always adds `--fork-session` for the CLIs that support
  it (`claude`, `puku-cli`). `omp` has no fork primitive and is refused
  for resume — see [Contract gaps](#contract-gaps).

These are checked before the child CLI runs. A calling agent that obeys this
file cannot accidentally bypass them.

### Large prompts (stdin-pipe path)

Prompts whose **byte length** exceeds `PROMPT_ARGV_LIMIT_BYTES` (128 KB =
131 072 bytes) are delivered via `--input-format stream-json --output-format
stream-json --verbose --replay-user-messages`, with a single
`{"type":"user","message":{"role":"user","content":…}}` envelope written to
the child's stdin. The argv slot that normally holds the prompt is empty;
the prompt body never enters argv, so the kernel's `ARG_MAX` cannot trigger
`E2BIG`. The safety contract (`--permission-mode plan`, `--fork-session`,
`--session-id`) is preserved on this path.

The threshold measures **bytes**, not UTF-16 code units. A prompt of 100 K
emoji (4 bytes each = 400 KB on the wire) routes to stdin even though
`.length` is only 100 K. This is the difference between the bridge
correctly avoiding `E2BIG` and silently accepting a payload that will
later be rejected by the kernel.

Use this path for `review` mode against large diffs; it kicks in
automatically. The `--dry-run` JSON output exposes the chosen path as
`prompt.delivery` (`"argv"` or `"stdin"`) so a caller can verify which
argv shape will be used without invoking the child.

If the CLI selected for the call has `stdin: false` (or unset) in the
`AGENTS` map and the prompt exceeds the threshold, the bridge **throws**
rather than silently falling back to argv (which would risk `E2BIG`).
This is the path that rejects large prompts against `omp` today; see
[Supported CLIs](#supported-clis) for the table and [Contract
gaps](#contract-gaps) for the per-CLI exception list.

## Supported CLIs

| CLI | Read-only mode | Resume | Stdin pipe |
|---|---|---|---|
| `claude` | `--permission-mode plan` | `--resume <id> --fork-session` | `--input-format stream-json --replay-user-messages` |
| `puku-cli` | `--permission-mode plan` | `--resume <id> --fork-session` | `--input-format stream-json --replay-user-messages` |
| `omp` | `--approval-mode always-ask` | **rejected** (no fork primitive; see [Contract gaps](#contract-gaps)) | **rejected** (no stream-json; large prompts must use argv or `--extra` chunks) |

`claude` and `puku-cli` share the same argv shape, so they go through
the bridge's default builder. `omp` uses a per-CLI argv hook
(`AGENTS["omp"].build`) that translates the shared contract into omp's
vocabulary and refuses the contracts omp cannot honor (fork-on-resume,
stream-json stdin).

Adding a new CLI is one entry in `src/main.ts`'s `AGENTS` map:

- Same argv shape as `claude`/`puku-cli`: set `bin`, `identity`, `name`,
  `stdin: true` (after verifying `--input-format stream-json
  --replay-user-messages` accepts a single user-message envelope — see
  [Large prompts](#large-prompts-stdin-pipe-path)). No `build` hook needed.
- Different argv shape: set the same fields and add a `build` hook. The
  hook receives the shared contract values and returns the final argv
  array. It owns its own read-only mode, resume policy, and prompt
  delivery; if the CLI cannot honor a contract, throw rather than silently
  bypass. See `AGENTS["omp"].build` for the pattern.

The CLI is considered supported when its `AGENTS` entry pins every
behavior the caller can observe. Missing shapes mean large reviews (or
resume, for forks) hard-fail with a clear error rather than silently
degrading.

### Contract gaps

`omp` is a first-class CLI but its argv surface is narrower than the
shared contract, so two behaviours are refused rather than approximated:

- **`--resume` is rejected.** omp has no `--fork-session` flag and no
  `--session-id` flag; `--resume <id>` extends the prior session. Forcing
  a fresh session is the only safe path here. The bridge refuses with
  `omp has no --fork-session; resume would extend the prior session` and
  points at `omp --resume <id> --no-session` for callers who want the
  unsafe path explicitly.
- **Large prompts are rejected.** omp does not declare stdin support, so
  the bridge refuses to fall back to argv once the prompt exceeds
  128 KB. The caller must shrink the prompt (or pipe it via `--extra`
  chunks in separate bridge calls).

## Privacy

- `--dry-run` shows the argv shape with the prompt redacted by length only —
  the payload is never written.
- Subject file reads are bounded to 20 KB.
- `git diff` output is bounded to 1 MB; oversize output is truncated in
  place and the truncation is marked in the prompt.
- The wrapper does not read session JSONL or rank prior sessions.
- Treat child model output as a hint, not truth. Verify before changing code.

## When to use a worktree

`harness-bridge` is read-only by contract (each CLI's read-only mode is
forced on every invocation — `--permission-mode plan` for `claude` and
`puku-cli`, `--approval-mode always-ask` for `omp`), so it cannot damage
the caller's working tree. For a single review, that is enough.

For a **multi-turn session** (`--resume <id>` across commits), or a
**plan-then-implement** flow where the sibling CLI is asked to draft a patch,
isolate the work in a [`git worktree`](../using-git-worktrees/SKILL.md).
The worktree rule lives there; this skill only flags the case.

## What this skill does NOT do

- Session discovery or JSONL parsing.
- Auto-resume based on heuristic scoring.
- Auto-detection of "I meant the current repo".
- Pinned sessions by topic / tag.
- A general-purpose CLI wrapper for anything other than harness-bridge use.

If you need any of these, call the child CLI directly. The wrapper is the
safety contract, not a workflow.

## Verify

```bash
deno task verify
deno task run -- puku-cli ask "dry run?" --dry-run
```
