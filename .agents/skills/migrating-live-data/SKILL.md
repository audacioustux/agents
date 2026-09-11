---
name: migrating-live-data
description: Use when writing or reviewing a database migration that will run against a table holding real data — adding or dropping a column, changing a type, backfilling, renaming, or a full cutover between schemas or systems. Also when a migration passed on a restored copy, or when planning a maintenance window.
uses:
  - name: verifying-before-completion
    source: audacioustux/agents
---

# Migrating Live Data

A migration that works on a restored copy can still fail in production, because
the copy is not being written to while you migrate it. Everything below concerns
the difference.

## Every step needs an abort criterion and a rollback

A migration plan that lists steps without saying how each one can fail is not a
plan; it is an optimistic narrative. During the window, under time pressure, the
question is never "what is the next step" — it is "is what just happened normal,
and if not, can I still go back".

For each step, write down:

- **The abort criterion.** What observation means stop. Not "if it fails" — a
  specific check with a specific threshold, because the failures that matter are
  the ones that do not raise. A count mismatch, a rejected row without a decided
  reason, a duration that puts the remaining steps outside the window.
- **The rollback.** What returns the system to its previous state, and what it
  costs. "Truncate the new tables and resume writes on the old system" is a
  rollback. "Restore from backup" is an admission that there isn't one.

Then identify the **point of no easy return** and mark it. Typically a prefix of
the sequence is reversible by doing nothing at all — dry runs, copying cold data,
building indexes — and then one step commits you. Knowing which step that is
changes how much verification belongs before it, and it is the difference between
an aborted migration and an incident.

## Rehearse until the rehearsal is boring

Dry-run against a restored copy, repeatedly, and have each run emit a
reconciliation report rather than a pass/fail:

- Row counts in and out, per table.
- Rejected rows with the reason each was rejected.
- The decision taken for every ambiguous case.
- **The wall-clock duration of the steps that will run inside the window.**

That last one is the one teams skip, and it is why windows get blown. A migration
whose correctness is perfect and whose duration is unmeasured is an unbounded
outage. The rehearsal is also where you discover the rejected rows, and every
rejection needs a decided reason before the real run — a row rejected for an
unexamined reason during the window is a decision made under pressure.

## A copy taken early is stale by construction

To fit a window, you migrate what you can in advance. But the source keeps
changing until writes stop, so anything copied early is out of date the moment it
lands — not through error, but by design.

This breaks the obvious verification. Comparing the early copy against the source
*now* reports a mismatch that is expected and tells you nothing.

Pin the comparison instead: record a snapshot timestamp when the early copy
begins, and verify that the copy **matches the source as of that timestamp**. The
criterion is "matches as of T", never "matches now".

Anything that changed after T is caught by the later pass over hot data, which is
the pass that runs with writes stopped. Deciding which data is cold enough to
copy early is therefore a correctness decision, not just a performance one: a
class that keeps changing until cutover cannot be in the early pass.

## Identifier remapping is per edge, not per entity

When keys do not survive the move — a type change, a new generation scheme, a
merge of two sources — every foreign key must resolve through a mapping.

The intuitive mapping is per source row: old id to new id. It is wrong whenever
one source row becomes several destination rows, which is the normal outcome of
splitting a wide table into a normalised set.

If one old row becomes a parent plus several children, then an old key referenced
by one relation may need to resolve to the parent, and by another relation to a
specific child. One old key, two legitimate destinations.

Key the mapping by **(source table, destination table) edge**, not by source
entity. Then each foreign key resolves through the edge that matches its meaning.

A per-entity mapping is not a design you can partially get away with: it builds
cleanly, passes a small fixture, and hits not-null violations partway through the
real run — inside the window, after writes have stopped.

## Insert in dependency order, and verify per step

Foreign keys impose an order. Derive it from the constraint graph rather than from
the order tables appear in the schema, and write it down — during the window is
not the time to discover a cycle.

Where a genuine cycle exists, break it explicitly: insert with the nullable side
empty, then update. That is a deliberate two-phase write, not an accident.

Verify per step rather than at the end. A single verification pass after
everything has moved tells you that something is wrong; a per-step check tells you
which step did it, while the rollback for that step is still cheap.

## Expand and contract, for changes that cannot be atomic

A column change on a live system is three deployments, not one:

1. **Expand.** Add the new shape. Nothing reads it yet. Old and new code both run
   against this schema — which is the actual requirement, because during a rolling
   deploy both versions are live simultaneously.
2. **Backfill and dual-write.** Populate the new shape for existing rows while
   writing both. The backfill must be idempotent and resumable: it will be
   interrupted, and restarting it from the beginning may not be affordable.
3. **Contract.** Once nothing reads the old shape, remove it. This is a separate
   deployment, and it is the one that is safe to defer.

Collapsing these into one migration is what takes the site down: for the duration
of a rolling deploy, one version of the code is running against a schema shaped
for the other.

## Verify the migration moved what it claimed

A migration that ran without error is not a migration that worked.

- Compare counts per table, against the pinned timestamp where one applies.
- Check aggregate invariants that must hold across the boundary — sums, balances,
  the count of rows in each state. A migration can move every row and still
  corrupt a total.
- Sample rows and compare them field by field, including the ones that were
  transformed rather than copied.
- Confirm the rejected set is empty, or that every rejection has its decided
  reason.

Absence of errors is not evidence. See `verifying-before-completion`.

## Correctness checks cannot see a cold optimiser

A bulk load leaves no query-planner statistics behind. Every table you just
filled has default estimates, so the planner chooses sequential scans over
indexes that exist and are perfectly good — at the moment traffic arrives.

This is invisible to everything above. Row counts match. Checksums match. Every
invariant holds. The data is correct and the system is unusably slow, because
correctness verification and plan quality are orthogonal and nothing in the first
reports on the second.

Refresh statistics on every loaded table as an explicit step, before traffic is
cut over and before the go decision — a migration that is correct but will fall
over under load is not a go. Where the engine offers a staged refresh that
produces rough estimates quickly and refines them after, prefer it: a cutover
window is waiting on this step.

It is easy to forget because adjacent operations do not need it. An in-place
engine upgrade typically carries statistics across. A restore from a logical dump
does not, and neither does a migration that inserts rows — which is what this is.

## Review checklist

- Does every step have an abort criterion that names a specific observation?
- Is the point of no easy return identified?
- Has the in-window duration been measured, not estimated?
- Is any early-copied data verified against a pinned timestamp rather than "now"?
- Does any source row become more than one destination row, and is the mapping
  keyed per edge?
- Is the backfill idempotent and resumable?
- Can both the old and new code run against the intermediate schema?
- What query proves the data arrived intact, and has it been run?
- Have planner statistics been refreshed on every loaded table before traffic?
