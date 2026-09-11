---
name: allocating-finite-inventory
description: Use when concurrent requests compete for a finite pool — seats, licences, stock, rate-limit tokens, job slots, connection leases — and two winners is a correctness failure. Also when a claim can outlive the process that made it, or when an allocation looks correct in review but oversells under load.
uses:
  - name: designing-idempotent-boundaries
    source: audacioustux/agents
---

# Allocating Finite Inventory

Two requests, one remaining unit. Whatever you write must let exactly one win,
and must stay correct when the loser retries, when a process dies holding a
claim, and when the pool is contended by more writers than you tested with.

## The shape of the problem

Overselling is not a race you can test away. The window is microseconds wide and
opens only under load, so a design that is wrong here passes every local test and
fails in production at the worst moment — peak traffic, which is exactly when the
pool is contended.

Read this before writing the claim, not after the first oversell.

## Claim with a predicate, not with a read

The naive shape reads the count, decides, then writes:

```
rows = select remaining from pool where id = X     -- sees 1
if rows.remaining > 0:
    update pool set remaining = remaining - 1 where id = X
```

Both callers see 1. Both decrement. The pool goes to -1, or to 0 having sold two
units. The gap between the read and the write is the whole defect, and no amount
of care inside that gap closes it.

The correct shape carries the decision into the write, and uses the engine's own
affected-row count as the win/lose signal:

```
affected = update pool
           set remaining = remaining - 1
           where id = X and remaining > 0

if affected == 0: lost
```

Nothing is read first. The predicate `remaining > 0` is evaluated by the database
under the row lock it already took to perform the write, so exactly one of two
concurrent statements can satisfy it. The loser gets zero affected rows and finds
out by checking, which means **you must check** — an unchecked affected-row count
is the same bug with extra steps.

This generalises past SQL. Any store offering a conditional write — a
compare-and-set on a version, an atomic decrement that refuses below zero, a
conditional put on an ETag — gives you the same primitive. What you cannot do is
reconstruct it from a read followed by a write.

## The re-evaluation blind spot

This is the rule that costs real money, and it is invisible in review because the
broken version looks identical to the correct one.

A conditional update may have several legs:

```
update seat set status = 'HELD'
where id = X
  and status = 'FREE'                      -- reads the row's own column
  and not exists (select 1 from sale where seat_id = X)   -- correlated subquery
```

Both legs read as predicates. They do not behave the same way under contention.

**Legs that read the row's own columns are re-checked against the latest
committed row when the lock is finally granted.** A writer that blocked on a lock
wakes, sees the new value of `status`, and correctly fails.

**Correlated subqueries are not re-evaluated.** They ran under the statement's
original snapshot, taken before the lock was granted. A rival that committed a
matching row while you were blocked is invisible to that subquery, which still
reports "no sale exists" from a snapshot that predates the sale.

So the predicate that looks strictest — the one reaching into another table to be
certain — is the one that silently passes. One caller holds, another sells, both
statements report success.

Two remedies, in order of preference:

1. **Keep the decisive legs on the row you are locking.** Denormalise the fact you
   are gating on into a column of the row itself, so the re-check covers it. A
   status column that a sale sets is re-evaluated; a subquery against the sales
   table is not.
2. **Re-read in a fresh statement after the claim**, at an isolation level that
   takes a new snapshot, and release the claim if the second read disagrees. This
   is correct but costs a round trip and leaves a window you must handle.

Escalating the isolation level is a third option. Take it only when the claim
spans several statements that must all see one snapshot — a single conditional
write never needs it. Reaching for it to fix one statement converts a silent
oversell into serialisation failures you must now retry at every call site, for
a guarantee you needed in one place.

## A reservation that outlives its holder

A claim held in memory, or held by a row with no expiry, leaks the moment a
process dies between claiming and confirming. The unit is gone and nothing will
return it.

Make expiry **structural** — a column the claim carries and every reader filters
on — rather than a cleanup job's responsibility:

```
where status = 'HELD' and expires_at > now()
```

An expired hold is then invisible to every query that matters, whether or not
anything has swept it. The sweeper becomes an optimisation that reclaims space,
not a correctness dependency. Get this backwards — let the sweeper define
availability — and every sweeper outage is an inventory outage.

The sweeper still needs care: it competes with live claims for the same rows, so
it must use the same conditional-write discipline. A reaper that reads expired
rows and then deletes them will eventually delete a hold that was renewed in the
gap.

## Back the claim with a constraint

`modelling-domain-invariants` owns the general rule that an invariant belongs
where no writer can bypass it. Two constraints are specific to allocation and
worth naming here:

- A check that the remaining count cannot go negative. This is the backstop for
  the predicate above — if a claim ever slips past it, the transaction fails
  instead of the pool going negative.
- An exclusion constraint where claims occupy ranges rather than units, which
  catches overlap that uniqueness cannot express.

Either turns a silent oversell into a failed transaction naming the constraint.
That is a bug report instead of an incident, and it holds against writers you
will never review.

## Review checklist

- Is there a read that decides something the subsequent write depends on? That
  gap is the defect.
- Is the affected-row count checked, or discarded?
- Does any predicate reach into another table? If so, is it load-bearing for
  correctness, and does the design account for it not being re-evaluated?
- Can a claim outlive the process that made it, and does every reader filter on
  expiry rather than trusting a sweeper?
- Does a constraint enforce the invariant, or only the application code?
- What happens on retry after a lost claim — is the retry itself safe? See
  `designing-idempotent-boundaries`.
