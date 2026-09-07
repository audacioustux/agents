---
name: handing-off-work
description: Use when context is running out, when work must pass to a fresh session, or when the user asks for a handoff — compacts the conversation into a document another agent can pick up from.
argument-hint: "What will the next session be used for?"
---

# Handing Off Work

## Overview

A handoff document exists so the next agent can act without re-deriving what this
session already learned. Write down the reasoning that has no other home, and point
at everything that does.

## What to write

Summarise the conversation into a document a fresh agent can continue from: what the
goal is, what has landed, what is in flight, and what was tried and rejected. The
rejected paths matter most, because nothing else in the repository records them and
the next agent will otherwise retry them.

Include a "suggested skills" section naming the skills the next agent should invoke,
so the handoff routes work rather than only describing it.

Where the user supplied arguments, treat them as the next session's focus and tailor
the document to it. A handoff aimed at a known next step beats a general summary.

## What to leave out

Do not duplicate content that already lives in a PRD, plan, ADR, issue, commit, or
diff. Reference those by path or URL. A copy goes stale the moment its source moves,
and the next agent cannot tell which version it is holding.

Redact secrets and personal data: API keys, passwords, tokens, and anything
identifying a person. The document outlives the session and lands outside the
repository, where none of the usual protections apply.

## Where it goes

Save to the operating system's temporary directory, never the workspace. A handoff
is session scaffolding, and committing one leaves a stale summary in the tree.
