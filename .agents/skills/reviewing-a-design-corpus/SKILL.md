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

Reviewing a design document is not reviewing code. There is no compiler, no test
suite, and no runtime to contradict a confident sentence. A document can be
internally coherent, well argued, and wrong in ways nothing mechanical will catch.

`requesting-code-review` and `receiving-code-review` cover the code case. This is
the other one.

## Have a reader with none of the author's context say it back

The author knows more than the page says, and so do reviewers who sat in the
same conversations. Both fill the gaps without noticing. The implementer who
arrives later with the document and nothing else cannot, and careful reading by
people who already know does not find what that reader will miss.

Before anyone argues with the design, give the document to one reader who has
none of that context and ask it to restate, in its own words, **what the
document decides, why, and what it leaves open**, marking every guess as a
guess. Then compare the restatement with what the author meant. This runs beside
the numbers pass below, since neither needs anything from the other, and ahead
of every pass that argues.

The reader:

- **Is a fresh session, never a fork** of the session that drafted or reviewed.
  A fork restates the design correctly from memory, which is the failure under
  test.
- **Gets the document and the files it links, nothing else.** Not the drafting
  conversation, not the author's notes, not a paragraph of orientation.
- **Does not get the repository's agent instruction files** (`AGENTS.md`,
  `CLAUDE.md` and their kin) unless the document links them. They hold the
  team's vocabulary, so a reader who has them passes a document its real reader
  fails. Starting a session fresh keeps the conversation out and often lets
  these in, because many harnesses load them into every new session. Never tell
  the reader to read them first, and have it list what it was given besides the
  document, so that a term it could have taken from there is not counted as
  understood.
- **Restates and does not review.** Its opinion of the design belongs to later
  passes, and asking for both thins the restatement.

What counts as a finding:

- The restatement is **wrong**. The document says something other than was
  meant, or needs outside context to be read correctly.
- The reader **guessed or could not say**: a term, a reason, a referent such as
  "the old importer" or "the agreed threshold" that the document relies on and
  does not contain.
- The **open list differs**. Something the reader took as settled is still open
  for the author, or the reverse.
- The restatement **hands the document's phrase back** where an answer was due.
  "Because of the vendor's limit", repeated as the reason, is a guess in the
  document's words: ask which vendor and what limit.

The reader is ignorant of the project, not of the trade: a general engineering
term it happens not to know is not a finding, and neither is its view of whether
the design is good. A restatement that matches with nothing guessed is a result.
Record it and move on.

The fix for each finding belongs in the document or in a file it links, and it
says what the author meant: where the page was silent because nothing was
decided, the fix is to say it is open. Explaining the gap to the reader fixes
one reader, and that reader is then spent: after a substantial rewrite, use a
new one. Where nobody who holds the author's intent is at hand, the guesses
stand as findings on their own and the restatement goes to the author to
compare.

One reader is enough. A second finds mostly the same gaps at twice the cost,
and this is a cheap first check, not a panel. A reviewer who has seen none of
the context can be that reader, once, by writing the restatement before opening
anything but the document and its links.

## Verify the numbers before arguing with the conclusions

Design documents rest on measured claims: how many call sites exist, how large a
table is, what a benchmark showed, which files a pattern appears in. Those claims
are load-bearing — a recommendation built on "there are only three call sites" is
a different recommendation when there are thirty.

Make factual verification a **separate pass that runs first**. Check every number
and every file reference against the actual tree before engaging with any
argument. Mixing the two is how a reviewer spends an hour on a design debate
premised on a count that was wrong.

Wrong counts are almost never invented. They are stale, or scoped differently
than the reader assumes — measured weeks ago against a tree that has since
moved, or measured a subdirectory while the sentence implies the repository.
The fix is provenance: **every measured number carries the date it was taken and
the command that produced it.** A stale number with its command attached is
re-runnable and self-correcting; a bare number is a claim the reader must either
trust or re-derive from scratch. A file that states its own length is stale on
the next edit, and the reader has no way to know.

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
settled — a proposal quietly rewritten as a ruling has no decision behind it
anyone can find.

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

## A shipped corpus drifts from the code

The numbers pass checks a document when it is written. Once the design ships,
the code keeps moving and the document does not: a function is renamed, a
constraint is dropped "for now", a second writer appears. The document still
reads as authoritative, and the next proposal is built on it.

Run a drift pass before building on a shipped document, before showing one to
anyone who will rely on it, and whenever the consistency pass above runs:

1. **List every statement the document makes about the code**, not only its
   numbers: names (functions, tables, files, flags, events), invariants ("the
   only writer", "never negative", "the store enforces it"), flows ("through the
   outbox", "never reads the primary") and counts.
2. **Check each against the tree**, with the command recorded and the result
   set beside the document's own date, as the provenance rule above asks. Check
   an invariant by searching for its violation: "nothing calls billing
   directly" is tested by looking for callers, and finding the path the document
   describes proves nothing. A search bounds where you looked, not what exists:
   a write made through a helper does not match a search for the SQL.
3. **Report each mismatch as "the document says X, the code does Y, since
   `<commit>`".** The commit that made the difference carries its date and its
   stated reason, which is most of what the decision needs.
4. **Decide the fix per finding, never for the batch.** A rename amends the
   document. A constraint the document decided and the code dropped may be the
   bug. Neither "the code is in production" nor "the document was approved"
   settles which: the first says the code is current, the second that the
   document was true once. A measured statement is corrected by measuring again.
   A decided one changes only by a ruling, so until someone rules, the document
   keeps the ruling and says where the code departs from it.

Report which statements held and which were not checked, so that "no drift" can
be told from "not looked at".

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
reintroduce when demand is real rather than anticipated. Record what was cut
and why, or the same mechanism is proposed again next round.

## Review checklist

- Has one reader with none of the author's context, given only the document and
  the files it links, restated what it decides, why, and what it leaves open?
- Has every measured claim been verified against the current tree, in a pass of
  its own, before any design argument?
- Does each measured number carry its date and the command that produced it?
- Are measured, decided, and proposed statements distinguishable?
- Does any concept have two names, or any name two concepts?
- Does every cross-reference resolve, and does it name what it points at?
- Is any fact stated authoritatively in two places?
- Where the design has shipped: has each statement about the code been checked
  against the tree, and does each mismatch name its commit and carry its own
  decision, amend the document or fix the code?
- Is every stated criterion actually evaluable?
- Does any finding about one branch of a parallel design assume the other behaves
  the same way?
- Has each decision been swept against the quality dimensions? Use the
  `weighing-quality-dimensions` skill if available, which owns the sweep; otherwise
  ask of each decision what it makes worse and whether the document says so.
