# Drift pass: reasoning and limits

Background for "A shipped corpus drifts from the code" in `../SKILL.md`.

The numbers pass checks a document when it is written. Once the design ships the
code keeps moving and the document does not: a function is renamed, a
constraint is dropped "for now", a second writer appears. The document still
reads as authoritative, and the next proposal is built on it.

## Statement classes to list

- **Names:** functions, tables, files, flags, events.
- **Invariants:** "the only writer", "never negative", "the store enforces it".
- **Flows:** "through the outbox", "never reads the primary".
- **Counts.**

## Checking

Check an invariant by searching for its violation. "Nothing calls billing
directly" is tested by looking for callers; finding the path the document
describes proves nothing. A search bounds where you looked, not what exists: a
write made through a helper does not match a search for the SQL.

## Why report the commit

The commit that made the difference carries its date and its stated reason,
which is most of what the decision needs.

## Why the fix is decided per finding

- "The code is in production" says the code is current; "the document was
  approved" says it was true once. Neither says which side is right.
- A constraint the document decided and the code dropped may be the bug.
- A measured statement is corrected by measuring again.
- A decided statement changes only by a ruling, so until someone rules the
  document keeps the ruling and says where the code departs from it.

## Reporting

Say which statements held and which were not checked, so that "no drift" can be
told from "not looked at".
