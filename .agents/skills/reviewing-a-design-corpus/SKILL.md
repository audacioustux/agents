---
name: reviewing-a-design-corpus
description: Use when reviewing a design document or a set of them — an architecture proposal, a migration plan, a multi-chapter spec — rather than code. Also when a document set has grown inconsistent, when a stated criterion cannot be evaluated, or when a measured claim in a document needs checking.
uses:
  - name: requesting-code-review
    source: audacioustux/agents
---

# Reviewing a Design Corpus

Reviewing a design document is not reviewing code. There is no compiler, no test
suite, and no runtime to contradict a confident sentence. A document can be
internally coherent, well argued, and wrong in ways nothing mechanical will catch.

`requesting-code-review` and `receiving-code-review` cover the code case. This is
the other one.

## Verify the numbers before arguing with the conclusions

Design documents rest on measured claims: how many call sites exist, how large a
table is, what a benchmark showed, which files a pattern appears in. Those claims
are load-bearing — a recommendation built on "there are only three call sites" is
a different recommendation when there are thirty.

Make factual verification a **separate pass that runs first**. Check every number
and every file reference against the actual tree before engaging with any
argument. Mixing the two is how a reviewer spends an hour on a design debate
premised on a count that was wrong.

There is a strong pattern in how these claims fail, and it shapes what to look
for:

**Wrong counts are almost never invented. They are stale, or scoped differently
than the reader assumes.** Someone measured accurately weeks ago against a tree
that has since moved, or measured a subdirectory while the sentence implies the
repository.

Neither failure is carelessness, and neither is fixed by measuring more carefully
— the number will drift again. The fix is provenance: **every measured number
carries the date it was taken and the command that produced it.** A stale number
with its command attached is re-runnable and self-correcting. A bare number is a
claim the reader must either trust or re-derive from scratch.

The same applies to a count describing the document that contains it. A file that
states its own length is stale on the next edit, and the reader has no way to know.

## Separate what was measured from what was decided

A mature design corpus accumulates three kinds of statement, and they are easy to
mix:

- **Measured** — a fact about the world, true when taken, re-checkable.
- **Decided** — a ruling. True because someone chose it, and changeable only by
  another ruling.
- **Proposed** — an option under consideration, not yet either of the above.

When these share a table or a paragraph without distinction, readers act on
proposals as if they were rulings, and re-litigate rulings as if they were
proposals. Implementers cannot tell which statements they are allowed to
contradict with evidence.

Keep them visibly distinct, and when a proposal is settled, record where it was
settled. A proposal quietly rewritten as a ruling has no decision behind it that
anyone can find, so the next person to disagree reopens it — and neither of them
can tell whether it was ever actually decided.

## What a document set drifts into

Inconsistencies that a single document cannot have appear as soon as there are
several, and none of them are caught by reading any one file:

- **Terminology drift.** The same concept acquires two names in different
  chapters, or worse, one name is used for two concepts. This is the most
  expensive kind, because readers do not notice — they assume the two names are
  two things, and design accordingly.
- **Cross-reference rot.** A section is renumbered or moved; every pointer to it
  now lands somewhere plausible and wrong. Pointers with section numbers rot
  faster than pointers naming what they point at.
- **Duplicated facts diverging.** The same rule stated in two chapters, updated in
  one. Both now read as authoritative.
- **Index drift.** The index lists what existed when it was written.

For each, the fix is ownership rather than vigilance: **one home per fact, and
every other mention points at it.** When a rule appears in two chapters, name one
canonical and delete the other — do not reconcile them into a third version.

Run a consistency pass over the whole set deliberately, on a schedule. None of
these defects are visible from inside the document that contains them, so no
amount of careful per-document review finds them — they accumulate until a
reader acts on the stale half.

## Check that a gate can be evaluated

Design documents state criteria: thresholds to meet, conditions to satisfy before
proceeding, limits not to exceed. A criterion citing a quantity nothing defines is
not a gate — it is a sentence that reads like one, and it will be marked satisfied
by whoever needs to proceed.

For every stated criterion: is the quantity defined, is it measurable, and is it
stated who measures it and when? If not, the gate is decorative.

## A finding can be right for one branch and wrong for another

The most valuable lesson available from a multi-round review is what later rounds
overturn — and the characteristic failure is not a wrong conclusion. It is a
conclusion that is correct for the case examined and silently assumed to hold for
a parallel case with different mechanics.

A reviewer checks one implementation of a two-implementation feature, finds the
justification for an exception unfounded, and concludes the exception is
unjustified. That conclusion holds for the branch examined. The other branch needs
the exception for an entirely different reason the reviewer never reached.

When a design has parallel paths — two inventory kinds, two payment rails, two
storage backends — a finding about one is a finding about one. Either check the
others or scope the finding explicitly to what was examined.

## Simplification is a separate pass with its own criterion

Removing unneeded design is not the same activity as reviewing it, and it does not
happen as a side effect. Run it deliberately, and apply one test to each mechanism:

**Is its cost justified by demand you can point at, and if it is cut, can it be
added later behind a seam that already exists?**

Both halves matter. Unjustified cost alone does not license removal if adding it
back later means restructuring. A clean seam alone does not justify keeping
something nobody has asked for. Together they identify what to cut now and
reintroduce when demand is real rather than anticipated.

Record what was cut and why. Otherwise the same mechanism is proposed again next
round, and the argument runs from the start.

## Review checklist

- Has every measured claim been verified against the current tree, in a pass of
  its own, before any design argument?
- Does each measured number carry its date and the command that produced it?
- Are measured, decided, and proposed statements distinguishable?
- Does any concept have two names, or any name two concepts?
- Does every cross-reference resolve, and does it name what it points at?
- Is any fact stated authoritatively in two places?
- Is every stated criterion actually evaluable?
- Does any finding about one branch of a parallel design assume the other behaves
  the same way?
