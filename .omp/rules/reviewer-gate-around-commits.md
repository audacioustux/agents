---
name: reviewer-gate-around-commits
description: "Require a reviewer-agent pass before major task execution and after every commit"
condition:
  # `git commit` carrying a message: -m, -F, --message, --file
  - 'git\s+commit\s+(-F|-m|--file|--message)'
  # `git commit` with an explicit pathspec: the `-- <paths>` form
  - 'git\s+commit\b[^\n]*--\s'
scope: "tool:bash"
---

## Read every deleted line before committing

```bash
git diff --cached | grep '^-'
```

Confirm you meant to remove each one. An `Edit` whose line range is wider
than its replacement body deletes the excess silently — a lead-in, a fence,
the tail of a sentence — and every repo gate stays green.

This is the whole structural check. Git knows exactly what was removed;
nothing needs to infer it from the result.

## Reviewer agent is a gate, not an option

Major task = multi-file edit, new section, mechanism replacement, schema
addition, or landing agent findings.

**Before:** state what changes and why, **read** the reply, answer the
objections, then edit. One dispatch with no reply round is not a
conversation. Zero pushback usually means the brief was too vague.

**After:** dispatch over the committed diff (`git show <sha>`), not your
summary of it.

Ask reviewers for judgement — wrong claims, missing registrations, bad
design. Do not ask them to re-verify a measurement you can run yourself;
that is slower and less reliable than running it.

## A green gate is not a review

Whatever this project's checks are — linters, type checks, link checkers,
schema validators, test suites — they answer "is it well-formed?", never
"is it right?". They stay green on a deleted heading, an inverted fence
pair, a duplicated paragraph, and a correct-looking claim that is false.

Run them because a red one is cheap information. Never read a green one as
a review having happened.

## If a commit already landed unreviewed

Review it now; land the correction as a follow-up. Do not carry the debt
into the next task.
