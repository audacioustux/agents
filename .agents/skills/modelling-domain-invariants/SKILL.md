---
name: modelling-domain-invariants
description: Use when designing or reviewing a database schema, a domain model, or a state machine, and deciding what the store enforces versus what application code checks. Also when a status or lifecycle value appears unreachable, when a nullable column's meaning is unclear, or when the same fact is stored in two places.
uses:
  - name: allocating-finite-inventory
    source: audacioustux/agents
---

# Modelling Domain Invariants

A schema is the set of facts that future code cannot get wrong. Everything else
is a convention, and conventions are broken by the next person, the migration
script, the fixture, and the raw-SQL escape hatch someone adds under deadline.

## Put the invariant where it cannot be bypassed

The question for every rule is not "is this checked" but "who can write a row
that violates it".

Application-level validation guards one path. The paths it does not guard are the
ones that produce the corrupt rows: bulk imports, migrations, admin tooling,
another service sharing the database, a fixture, a raw statement written to work
around an ORM limitation.

Where the store can express a rule declaratively, express it there:

- A check constraint for a domain rule — a value that cannot be negative, a
  vocabulary a column is restricted to, a pair of columns whose relationship is
  fixed.
- A uniqueness constraint for anything that means "only one".
- A foreign key for a reference that must resolve, and an explicit decision about
  what deletion does rather than whatever the default is.
- Not-null on anything the code already assumes is present. A nullable column that
  is never null in practice is a null check every reader must write and some will
  forget.

Defaults deserve particular care: a default that lives in the application fires
only for writers that go through the application. A default in the store fires
for every writer, including the ones you did not anticipate. That difference is
invisible until a migration inserts a row without the column and lands a null
where the code proved it could never be.

## Each rule carries the failure that caused it

A list of schema rules without reasons is a list someone will relitigate — and
eventually relax, because the cost of the rule is visible in the moment and the
cost of breaking it is not.

Write each rule with the specific failure it prevents, ideally one that has
actually happened:

> Every primary key has a storage-level default. A process that inserts without
> one lands a null on a missing default, and every concurrent writer produces the
> same constraint violation at once.

> Every timestamp carries a time zone. Users, operators, and third parties are in
> different zones; a bare local timestamp is a silent correctness bug that
> surfaces as an off-by-hours discrepancy nobody can reproduce.

This changes how the rule is read. "Always store a time zone" invites a debate about
preference. "A bare timestamp silently produced an off-by-hours bug here" ends
it. The reason is also what lets a future reader judge when the rule genuinely
does not apply, rather than guessing.

## Every state needs a named writer

For any state machine — an order status, a job lifecycle, a document workflow —
enumerate the states and, for each one, name the code that writes it.

A state nothing writes is not a harmless spare. It is unreachable, and **it
silently removes every path that runs through it.** If a later state's only legal
predecessor is a state nothing writes, that later state is unreachable too, and
any feature depending on it is specification describing something impossible. The
documentation will read correctly. The tests will pass, because they set the
state up directly rather than arriving at it.

When you find a writer-less state, there are two fixes and the cheap one is often
wrong:

- **Delete it**, if nothing reads it either. That is the right call for a state
  that was speculative.
- **Give it a writer**, if a reader already depends on the distinction it makes.
  Deleting a state that a consumer branches on does not simplify the model; it
  makes a question the consumer needs to answer unanswerable.

Apply the same test to every value in a constrained vocabulary. A check constraint
listing five values where only three are ever written is describing a model
nobody implemented.

## Model what is true, not what is convenient

- **A nullable column asserts that absence is meaningful.** If it is not, the
  column should be required. If it is, the meaning of null needs to be written
  down — "not yet supplied" and "known to be absent" are different facts and
  conflating them loses information no query can recover.
- **A soft delete is a state, not an absence.** Every query must now filter it,
  and the one that forgets is a data leak. Uniqueness constraints must decide
  whether a deleted row still occupies its key. Neither of these is optional once
  the column exists.
- **A boolean pair that cannot both be true is a single state column.** Two
  booleans admit four combinations and the model has three; the fourth will occur.
- **Denormalise deliberately, and name the writer.** A copied value is a second
  source of truth that drifts. That is sometimes the right trade — see
  `allocating-finite-inventory` for a case where it is required for correctness —
  but it needs an owner and a rule for staying consistent.

## One fact, one home

When the same fact is expressed in two places, they will disagree. The question is
only when, and which one the reader believes.

The same principle governs rules across skills (`writing-skills`) and facts across
the chapters of a design corpus (`reviewing-a-design-corpus`). Here it is about
stored data.

Decide which is canonical, and treat the other as a defect to delete rather than a
second opinion to reconcile. This applies to a value duplicated across tables, a
rule stated in both a constraint and a validator, and a fact documented in two
chapters.

The reconciliation instinct is the trap: merging two divergent copies produces a
third version and leaves both originals in place.

## Review checklist

- For each invariant the code relies on: can a writer outside this code path
  violate it?
- Does each rule state the failure it prevents, specifically enough to judge when
  it does not apply?
- For each state in each lifecycle: what writes it? What becomes unreachable if
  nothing does?
- Does every value in a constrained vocabulary have a writer?
- For each nullable column: is absence meaningful, and is its meaning recorded?
- For each denormalised value: who writes it, and what keeps it consistent?
- Is any fact expressed in two places without one being named canonical?
