---
name: designing-agent-friendly-clis
description: Use when building or reviewing a command-line tool that agents, scripts, or CI will invoke — flags, help text, exit codes, errors, idempotency, dry-run, and machine-readable output. Stack-agnostic.
---

# Designing Agent-Friendly CLIs

## Role

Shape a CLI so a non-interactive caller can discover it, drive it, and tell what
happened. The caller may be an agent, a CI job, or a shell script; none of them can
answer a prompt, and all of them read the exit code.

## Every input reachable without a prompt

An interactive prompt is a hang for anything that is not a person. Each value the
command needs must be expressible as a flag or an argument, and a missing one must
fail rather than wait.

Prompting is a fallback for a human on a terminal, not the primary path. Build the
flag route first and offer the prompt when the flag is absent and stdin is a TTY; the
reverse order leaves the automated caller with no route at all.

## Discoverable in layers

Help is read one subcommand at a time. Print the top-level command list at the root
and let each subcommand document itself, so a caller learns the part it needs without
paying for the manual it does not.

Every `--help` carries worked examples, not just an option table. An example fixes
flag order, value shape, and quoting in one line, where prose leaves each of those to
be guessed.

## Errors that say what to run

A failure message that names only what was wrong forces the caller to search. Name
the correction: the flag that was missing, a valid invocation, and where the accepted
values come from.

Fail on the first missing input rather than partway through the work. A command that
does half its job and then discovers a missing flag leaves the caller to work out
what was already applied.

## Exit codes carry the verdict

The exit code is the only part of the result every caller reads without parsing.
Zero means the command did what was asked; non-zero means it did not. A tool that
exits zero while printing an error to stderr is invisible to `set -e`, to CI, and to
any retry logic wrapping it.

Reserve distinct non-zero codes where the caller would act differently — not found,
denied, invalid usage — so retrying is a decision rather than a guess.

## Safe to run twice

Automated callers retry: after a timeout, after a partial failure, or because a step
re-ran. A command that succeeds should be safe to repeat, either as a no-op or by
reporting that the work was already done.

Where an operation genuinely cannot be repeated, say so in the failure rather than
performing it twice.

## Preview before destruction

A destructive command needs a way to show its plan without executing it, and a way
to skip confirmation without a keystroke. Keep the confirmation as the default for
the human path, and let `--dry-run` and an explicit `--yes` cover the automated one.

A destructive default with no preview forces the caller to choose between running
blind and not running at all.

## Predictable shape across commands

One pattern for every command — resource then verb, or verb then resource, but not
both — lets a caller generalise from one subcommand to the next. Mixed shapes mean
every command has to be looked up.

## Output a caller can use

Success output should carry the identifiers the next step needs: ids, urls,
durations, paths. Decoration that only reads well in a terminal is unusable to
whatever comes next in the pipeline.

Offer a machine-readable mode where output will be parsed, and keep it on stdout with
diagnostics on stderr, so a pipeline reading stdout gets data rather than progress
chatter.

## Review checklist

- Can every input be supplied without a prompt, and does a missing one fail fast?
- Does each subcommand's `--help` carry a runnable example?
- Does every error name the correction, not just the problem?
- Does the exit code disagree with the output anywhere — zero on failure, or the reverse?
- Is a successful command safe to run a second time?
- Does a destructive path offer both a preview and a non-interactive confirmation?
- Do diagnostics go to stderr, leaving stdout parseable?

## Anti-patterns

- A required value that can only be supplied by answering a prompt.
- Dumping the full manual on every invocation, including successful ones.
- Exiting zero after printing an error, so callers treat a failure as success.
- An error naming what was wrong without naming what to run instead.
- Progress output on stdout, so anything parsing the result reads spinner frames.
- Destructive operations with no dry run, forcing the caller to test in production.
