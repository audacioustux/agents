# Document sets, parallel branches, simplification: reasoning

Background for four sections of `../SKILL.md`.

## What a document set drifts into

Inconsistencies that a single document cannot have appear as soon as there are
several, and none is caught by reading one file.

- **Terminology drift** is the most expensive kind, because readers do not
  notice: they assume the two names are two things, and design accordingly.
- **Cross-reference rot:** pointers with section numbers rot faster than
  pointers naming what they point at.
- **Duplicated facts diverging:** both now read as authoritative.

When a rule appears in two chapters, name one canonical and delete the other;
reconciling them yields a third version. None of these defects is visible from
inside the document that contains them, so they accumulate until a reader acts
on the stale half.

## Numbers

Wrong counts are almost never invented. They were measured weeks ago against a
tree that has since moved, or measured a subdirectory while the sentence implies
the repository. A stale number with its command attached is re-runnable and
self-correcting; a bare one must be trusted or re-derived from scratch. A file
that states its own length is stale on the next edit. Mixing the numbers pass
with argument is how a reviewer spends an hour on a debate premised on a wrong
count: "only three call sites" is a different recommendation at thirty.

## Measured, decided, proposed

When the three share a table without distinction, readers act on proposals as
rulings and re-litigate rulings as proposals, and implementers cannot tell which
statements they may contradict with evidence.

## A gate that cannot be evaluated

A criterion citing a quantity nothing defines is a sentence that reads like a
gate.

## A finding right for one branch

The characteristic failure of a multi-round review is a conclusion correct for
the case examined and silently assumed for a parallel case with different
mechanics. Example: a reviewer checks one implementation of a two-implementation
feature, finds the justification for an exception unfounded, and concludes the
exception is unjustified. That holds for the branch examined; the other branch
needs the exception for a different reason the reviewer never reached.

## Simplification

Unjustified cost alone does not license removal if adding it back means
restructuring. A clean seam alone does not justify keeping something nobody has
asked for. Together they identify what to cut now and reintroduce when demand is
real rather than anticipated.
