---
name: reviewing-a-design-corpus
description: Use when reviewing a design document or a set of them — an architecture proposal, a migration plan, a multi-chapter spec — rather than code. Also when a document set has grown inconsistent, when a stated criterion cannot be evaluated, when a measured claim in a document needs checking, when asking whether a spec is self-contained enough to hand to someone new, or whether shipped design documents still match the code.
uses:
  - name: requesting-code-review
    source: audacioustux/agents
  - name: weighing-quality-dimensions
    source: audacioustux/agents
---

# Reviewing a Design Corpus

Reviewing a design document is not reviewing code: nothing mechanical
contradicts a confident sentence. `requesting-code-review` and
`receiving-code-review` cover the code case. This is the other one.

## Have a reader with none of the author's context say it back

Run this beside the numbers pass and ahead of every pass that argues. Give the
document to one reader and ask it to restate, in its own words, **what the
document decides, why, and what it leaves open**, marking every guess as a
guess. Compare the restatement with what the author meant.

The reader:

- **Is a fresh session, never a fork** of the session that drafted or reviewed.
- **Gets the document and the files it links, nothing else**: not the drafting
  conversation, the author's notes, or a paragraph of orientation.
- **Does not get the agent instruction files** (`AGENTS.md`, `CLAUDE.md` and
  kin) unless the document links them. A fresh session often loads them anyway:
  never tell the reader to read them, and have it list what it was given besides
  the document, so a term it could have taken from there is not counted as
  understood.
- **Restates and does not review.** Its opinion of the design is for later passes.

A finding is:

- a **wrong** restatement: the document says other than was meant, or needs
  outside context to be read correctly;
- a **guess or "could not say"**: a term, reason or referent ("the old
  importer") the document relies on and does not contain;
- an **open list that differs** from the author's;
- the document's **own phrase handed back** where an answer was due ("because of
  the vendor's limit": ask which vendor and what limit).

An engineering term the reader happens not to know is not a finding, nor is its
view of the design. A matching restatement with nothing guessed is a result:
record it and move on.

The fix goes in the document or a file it links and says what the author meant;
where nothing was decided, say it is open. Do not explain the gap to the reader:
that reader is spent, so after a substantial rewrite use a new one. Where nobody
holds the author's intent, the guesses stand as findings and the restatement
goes to the author. Use one reader, not a panel; a reviewer who has seen none of
the context can be that reader once, writing the restatement before opening
anything but the document and its links. Why each rule, with examples:
`references/reader-restatement.md`.

## Verify the numbers before arguing with the conclusions

Make verification of measured claims (call sites, table sizes, benchmarks,
which files hold a pattern) a **separate pass that runs first**: check every
number and file reference against the actual tree before engaging with any
argument. Wrong counts are stale or scoped differently than they read, so
**every measured number carries the date it was taken and the command that
produced it**, and a file does not state its own length.

## Separate what was measured from what was decided

- **Measured**: a fact about the world, true when taken, re-checkable.
- **Decided**: a ruling, changeable only by another ruling.
- **Proposed**: an option under consideration, neither of the above.

Keep the three visibly distinct, and when a proposal is settled, record where.
A proposal quietly rewritten as a ruling has no decision anyone can find.

## What a document set drifts into

Look for these across files:

- **Terminology drift**: two names for one concept, or one name for two.
- **Cross-reference rot**: a renumbered section leaves pointers landing
  somewhere plausible and wrong. Name what a pointer targets, not its number.
- **Duplicated facts diverging**: one rule in two chapters, updated in one.
- **Index drift**: the index lists what existed when it was written.

The fix is **one home per fact, every other mention pointing at it**. Name one
canonical and delete the other; do not reconcile two into a third. Run a
consistency pass over the whole set deliberately, on a schedule, since careful
per-document review cannot find these. Detail: `references/document-set-and-judgement.md`.

## A shipped corpus drifts from the code

Run a drift pass before building on a shipped document, before showing one to
anyone who will rely on it, and whenever the consistency pass runs:

1. **List every statement the document makes about the code**: names,
   invariants ("the only writer"), flows ("through the outbox") and counts.
2. **Check each against the tree**, recording the command and setting the result
   beside the document's date. Check an invariant by searching for its
   violation: finding the path the document describes proves nothing.
3. **Report each mismatch as "the document says X, the code does Y, since
   `<commit>`"**.
4. **Decide the fix per finding, never for the batch.** A rename amends the
   document. A decided constraint the code dropped may be the bug; neither "in
   production" nor "was approved" settles which. A measured statement is
   corrected by measuring again; a decided one changes only by a ruling, so
   until then the document keeps it and says where the code departs.

Report which statements held and which were not checked, so "no drift" can be
told from "not looked at". Examples and limits of the search: `references/drift-pass.md`.

## Check that a gate can be evaluated

For every stated criterion, ask: is the quantity defined, is it measurable, and
is it stated who measures it and when? If not, the gate is decorative and
whoever needs to proceed will mark it satisfied.

## A finding can be right for one branch and wrong for another

When a design has parallel paths (two inventory kinds, two payment rails, two
storage backends), a finding about one is a finding about one. Check the others
or scope the finding explicitly to what was examined. Example:
`references/document-set-and-judgement.md`.

## Simplification is a separate pass with its own criterion

Run it deliberately. Ask of each mechanism: **is its cost justified by
demand you can point at, and if it is cut, can it be added later behind a seam
that already exists?** Both halves matter: cost without a seam does not license
removal, and a clean seam does not justify keeping what nobody asked for. Record what was cut and why, or it is proposed again next round.

## Review checklist

- One context-free reader restated what the document decides, why, and what is open?
- Every measured claim verified against the tree, first, with its date and command?
- Measured, decided and proposed statements distinguishable?
- No concept with two names, no name with two concepts, no fact with two homes?
- Every cross-reference resolves and names its target?
- Shipped: each statement about the code checked, each mismatch with its commit and its own decision?
- Every stated criterion evaluable?
- No finding about one branch assumed to hold for the other?
- Each decision swept against the quality dimensions: use `weighing-quality-dimensions` if available, which owns the sweep; otherwise ask what it makes worse and whether the document says so.
