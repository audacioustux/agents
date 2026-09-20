---
name: harness-bridge
description: Use when an agent needs to invoke a sibling agent CLI — currently `claude` or `puku-cli` — under a small safety contract: `--continue/-c` is rejected, `--permission-mode plan` is forced, `--fork-session` is forced on resume. The bridge is general-purpose (any task the caller would otherwise pass to a CLI), not limited to "second opinion" framing. Adding a new CLI is a one-line entry if it accepts the same argv shape. Not for invoking a CLI without the safety contract (call the CLI directly); not for parsing or scoring session JSONL (the bridge does not read session stores).
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
| `--model <name>` | Override model (honoured by both `claude` and `puku-cli`) |
| `--base <ref>` / `--head <ref>` | Diff range for `review` mode (default `HEAD~1`..`HEAD`) |
| `--extra "..."` | Additional instructions appended to the prompt |

## Safety contract

The wrapper rejects or forces the following at parse time:

- `--continue` and `-c` are **rejected**. They resume the most recent session
  globally, ignoring the current repo. Use `--resume <id>`.
- `--permission-mode plan` is **always added**. Both `claude` and `puku-cli`
  accept the flag; without it the model can write to the repo.
- `--resume <id>` always adds `--fork-session`. Without it, the child CLI
  extends the prior session with the new prompt.

These are checked before the child CLI runs. A calling agent that obeys this
file cannot accidentally bypass them.

### Large prompts (stdin-pipe path)

Prompts whose byte length exceeds `PROMPT_ARGV_LIMIT` (128 KB) are delivered
via `--input-format stream-json --output-format stream-json --verbose
--replay-user-messages`, with a single `{"type":"user","message":{"role":"user","content":…}}`
envelope written to the child's stdin. The argv slot that normally holds the
prompt is empty; the prompt body never enters argv, so the kernel's `ARG_MAX`
cannot trigger `E2BIG`. The safety contract (`--permission-mode plan`,
`--fork-session`, `--session-id`) is preserved on this path. Use this path
for `review` mode against large diffs; it kicks in automatically.

## Supported CLIs

| CLI | Notes |
|---|---|
| `claude` | Anthropic's Claude Code CLI |
| `puku-cli` | Puku CLI (Claude-compatible surface; routed to OmniRoute-managed models) |

Adding a new CLI is a one-line entry in `src/main.ts`'s `AGENTS` map — both
CLIs share the same argv shape (`-p --permission-mode plan [--model M]
[--name N] [--resume ID --fork-session | --session-id UUID] <prompt>`)
and accept the same `--input-format stream-json --replay-user-messages`
stdin envelope (see [Large prompts](#large-prompts-stdin-pipe-path)). The CLI
is considered supported if it accepts both shapes; missing the stdin shape
means large reviews will silently fall back to argv and may hit `E2BIG`.

## Privacy

- `--dry-run` shows the argv shape with the prompt redacted by length only —
  the payload is never written.
- Subject file reads are bounded to 20 KB.
- `git diff` output is bounded to 1 MB; oversize output is truncated in
  place and the truncation is marked in the prompt.
- The wrapper does not read session JSONL or rank prior sessions.
- Treat child model output as a hint, not truth. Verify before changing code.

## When to use a worktree

`harness-bridge` is read-only by contract (`--permission-mode plan` is forced
on every invocation), so it cannot damage the caller's working tree. For a
single review, that is enough.

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
