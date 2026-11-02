---
name: harness-bridge
description: 'Use when an agent needs to invoke a sibling agent CLI — currently `claude`, `puku-cli`, or `omp` — under a small safety contract: `--continue/-c` is rejected, `--permission-mode plan` (or its equivalent) is forced, `--fork-session` is forced on resume where the CLI supports it. The bridge is general-purpose (any task the caller would otherwise pass to a CLI), not limited to "second opinion" framing. Adding a new CLI is one `Agent` variant if it accepts the same argv shape, or a small per-CLI argv builder if it does not. Not for invoking a CLI without the safety contract (call the CLI directly); not for parsing or scoring session JSONL (the bridge does not read session stores).'
---

# harness-bridge

Bridge from one agent to a sibling agent CLI under a tight safety contract.
The wrapper is a small Rust binary. Its only job is to make a few unsafe
invocations *physically impossible* — see [Safety contract](#safety-contract).
The bridge is **general-purpose**: it does not assume the caller's task is a
"review" or "second opinion." Review-flavoured prompts are one of several
modes, not the frame.

## Run it

Install it once (from this directory):

```bash
cargo install --path . --locked      # puts `harness-bridge` in ~/.cargo/bin
```

```bash
harness-bridge claude ask "challenge this approach"
harness-bridge puku-cli adversarial docs/plans/migration.md
harness-bridge omp ask "challenge this approach"
harness-bridge claude review --base main --head HEAD
harness-bridge puku-cli ask "is this safe?" --fresh --dry-run
```

Without installing, `cargo run --quiet --manifest-path <this dir>/Cargo.toml -- claude ask "..."`
does the same (the `--` is consumed by cargo).

Unknown flags are errors. Text after a bare `--` is part of the subject, so
`harness-bridge claude ask -- what does --continue do` asks about the flag.

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
- `--permission-mode plan` is **always added** for `claude` and `puku-cli`
  (read-only by capability — the model cannot write). `omp` has no
  read-only mode; it is forced into `--approval-mode always-ask` instead,
  which is **approval-gated** (the user must approve every tool call)
  but the model is still *capable* of writing if the user approves. This
  is a permanent gap in omp's argv surface — there is no `plan` analogue.
  See [Contract gaps](#contract-gaps).
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

If the CLI selected for the call does not take stdin (`Agent::takes_stdin`
in `src/lib.rs`) and the prompt exceeds the threshold, the bridge **refuses**
rather than silently falling back to argv (which would risk `E2BIG`).
This is the path that rejects large prompts against `omp` today; see
[Supported CLIs](#supported-clis) for the table and [Contract
gaps](#contract-gaps) for the per-CLI exception list.

## Supported CLIs

| CLI | Read-only mode | Resume | Stdin pipe |
|---|---|---|---|
| `claude` | `--permission-mode plan` | `--resume <id> --fork-session` | `--input-format stream-json --replay-user-messages` |
| `puku-cli` | `--permission-mode plan` | `--resume <id> --fork-session` | `--input-format stream-json --replay-user-messages` |
| `omp` | `--approval-mode always-ask` (approval-gated, not read-only — see [Contract gaps](#contract-gaps)) | **rejected** (no fork primitive) | **rejected** (no stream-json) |

`claude` and `puku-cli` share the same argv shape, so they go through
the bridge's default builder (`Call::plan_mode` in `src/bridge.rs`). `omp`
has its own builder (`Call::omp`) that translates the shared contract into
omp's vocabulary and refuses the contracts omp cannot honor (fork-on-resume,
stream-json stdin).

Adding a new CLI is one `Agent` variant in `src/lib.rs` (its `name`,
`identity` and `takes_stdin`), plus its arm in `Call::build`:

- Same argv shape as `claude`/`puku-cli`: route it to `plan_mode`. Claim
  `takes_stdin` only after verifying `--input-format stream-json
  --replay-user-messages` accepts a single user-message envelope — see
  [Large prompts](#large-prompts-stdin-pipe-path).
- Different argv shape: give it its own builder. It owns its own read-only
  mode, resume policy, and prompt delivery; if the CLI cannot honor a
  contract, return an error rather than silently bypass. See `Call::omp`
  for the pattern.

The compiler makes every `match` on `Agent` name the new variant, so no
code path can forget it. The CLI is considered supported when its entry pins every
behavior the caller can observe. Missing shapes mean large reviews (or
resume, for forks) hard-fail with a clear error rather than silently
degrading.

That table is a claim about software this repository does not control, and it
has no expiry. A CLI that renames a flag, gates it behind a new mode, or drops
it in a minor release leaves the entry describing a version that no longer
exists, and the bridge goes on building argv from it. The failure surfaces as
the child CLI's own usage error, attributed to the caller's request rather than
to a stale table.

So treat a capability as unproven until something observed it on the CLI
actually installed. `--dry-run` prints the argv the bridge would run without
invoking anything, which is the cheap version of that check: run it for each
contract a CLI claims to honour, and compare against what the CLI accepts
today. Do this when adding an entry, and again after any upgrade to a child
CLI — an entry that was true at the version it was written against says nothing
about the one on the machine.

A capability that cannot be demonstrated should be refused rather than
attempted. `omp` already shows the shape: its builder refuses
fork-on-resume and stream-json stdin instead of approximating them. Refusing a
contract the CLI cannot honour produces one clear error at the bridge; letting
it through produces a confusing one downstream.

### Contract gaps

`omp` is a first-class CLI but its argv surface is narrower than the
shared contract, so the following behaviours are refused or downgraded
rather than approximated:

- **`--resume` is rejected.** omp has no `--fork-session` flag and no
  `--session-id` flag; `--resume <id>` extends the prior session. Forcing
  a fresh session is the only safe path here. The bridge refuses with
  `omp has no --fork-session; resume would extend the prior session` and
  points at `omp --resume <id> --no-session` for callers who want the
  unsafe path explicitly (and accept the fork-on-resume gap).
- **Large prompts are rejected outright.** omp does not declare stdin
  support, so the bridge refuses to fall back to argv once the prompt
  exceeds 128 KB. The caller must shrink the prompt (or pipe it via
  `--extra` chunks in separate bridge calls — `--extra` itself has no
  helper, callers split manually).
- **Read-only is approval-gated, not capability-gated.** omp has no
  equivalent of `--permission-mode plan`. The bridge pins
  `--approval-mode always-ask`, which forces the user to approve each
  tool call but does not prevent the model from writing if the user
  approves. For tasks where the *capability* must be removed (e.g. unat-
  tended review of an untrusted repo), use `claude` or `puku-cli`; for
  attended review, `omp` is sufficient.

## Privacy

- `--dry-run` shows the argv shape with the prompt redacted by length only —
  the payload is never written.
- Subject file reads are bounded to 20 KB.
- `git diff` output is bounded to 1 MB; oversize output is truncated in
  place and the truncation is marked in the prompt.
- The wrapper does not read session JSONL or rank prior sessions.
- Treat child model output as a hint, not truth. Verify before changing code.

## When to use a worktree

`harness-bridge` is read-only by *capability* for `claude` and
`puku-cli` (the model cannot write to the repo), and approval-gated for
`omp` (every tool call must be approved by the user — the model is
*capable* of writing). The wrapper itself does not touch files, so a
single review against the caller's tree is safe in either mode. For a
**multi-turn session** (`--resume <id>` across commits), or a
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
cargo fmt --check && cargo clippy --all-targets && cargo test
harness-bridge puku-cli ask "dry run?" --dry-run
```

`cargo test` includes checks against the installed `omp` (its argv shape, and
that `--help` still lists every flag the bridge builds and the `--no-session`
escape hatch). They skip when `omp` is not installed.

## The library

The crate is also a library, `harness_bridge`, so tools that start agent
CLIs share one description of their flags instead of each keeping its own:

- `harness_bridge::bridge` — this skill's read-only contract.
- `harness_bridge::headless` — an unattended Claude Code session for a runner
  that provides its own boundary. [relay](../../../relay/README.md) uses it
  inside bubblewrap. It is *not* this skill's contract: its `Unrestricted`
  permission exists only for sandboxed sessions.
