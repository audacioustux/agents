---
name: modelling-domains-and-events
description: Use before designing or extending a backend whose feature spans more than one business concern - orders, inventory, payments, bookings, returns, billing - and before deciding which module a new concept belongs in. Also when modules share tables or pass one transaction between them, when business rules sit in handlers or services instead of the model, when an event type is published from two places, or when a database write and a message publish are not atomic. Covers DDD, bounded contexts, moduliths, aggregates, domain events, sagas, outbox.
uses:
  - name: modelling-domain-invariants
    source: audacioustux/agents
  - name: designing-idempotent-boundaries
    source: audacioustux/agents
  - name: evolving-shared-contracts
    source: audacioustux/agents
---

# Modelling Domains and Events

Most code is written without a domain model: tables first, a service per table,
rules wherever the request handler happened to need them. That shape works until
two concerns meet, and then every feature touches every table and every rule
holds only on the path someone remembered. Model the domain before writing code
for it, and make the model the code's structure.

This skill owns the model: language, module boundaries, aggregates, events and
the workflows between modules. What the store itself enforces inside one module
is `modelling-domain-invariants`.

## When to use

- A feature with two or more business concerns, greenfield or not.
- A new concept arriving in an existing codebase: decide its home before its
  columns.
- Reviewing a design where modules call each other inside one transaction, read
  each other's tables, or publish after commit.

For a single concern with one lifecycle, the model is short: a glossary of a few
terms and one aggregate. Write it anyway; it takes minutes. Small does not exempt
a design from the rules below; it only makes the artifacts short. Under a
deadline, compress the five artifacts to a line each; do not drop one.

## Produce these before code

1. **Glossary (ubiquitous language).** Each term, its meaning, and the context
   that owns it. Code, tables, endpoints and events use these words and no
   synonyms. One word meaning two things ("order" as a cart in sales, as a pick
   list in the warehouse) is a context boundary, not a naming problem.
2. **Context map.** Each bounded context is one module in one deployable (a
   modulith). For each: the tables it owns, and its surface - the commands it
   accepts, the queries it answers, the events it publishes. Nothing else is
   reachable from outside.
3. **Aggregates.** For each module, the aggregates, the invariants each holds,
   and the methods that change it. Rules live in those methods, not in handlers.
4. **Event catalogue.** Each event: past-tense name, the one module that emits
   it, its consumers, its version.
5. **Process managers.** Each workflow that crosses modules: its states, the
   events it reacts to, the commands it issues, its timeouts, and the
   compensation for every step that can be undone.

## Module boundaries are enforced, not described

Folders are not walls. A boundary holds only when crossing it fails.

- **A module owns its tables outright.** No other module reads, writes, joins or
  holds a foreign key into them. Give each module its own schema and database
  role, or at least a lint rule on imports; a shared database handle everyone
  imports is the boundary's absence.
- **No transaction spans two modules.** Passing a transaction handle into
  another module's function makes that module's tables part of your
  consistency boundary; neither can now change its schema, lock order or
  storage alone. If a rule seems to need one atomic write across two modules,
  either both pieces belong in one aggregate (redraw the boundary) or the rule
  is eventually consistent (model the in-between state). Pick one explicitly.
- **Another module's data arrives through its surface.** Query it, or keep a
  local read model fed by its events. A copy needs a named source, per
  `modelling-domain-invariants`.

**Where a new concept goes.** "It is about orders" is not a test; everything is
about orders. Give the concept its own context when it has its own lifecycle,
its own actor or its own words: a return is requested, inspected, approved or
rejected, by a warehouse operator, and has a quantity and a reason the order
never had. A status column on someone else's table is the symptom of skipping
this question.

## Aggregates are consistency boundaries

An invariant holds inside one aggregate, in one transaction. One transaction
changes one aggregate. Across aggregates and across modules, consistency is
reached through events, and the time in between is a state the model names
(`PendingPayment`, `ReservationHeld`), not a gap it hopes is short.

A model whose classes are only fields, with every rule in a service or handler,
is anemic: the rule holds on the path that calls the service and nowhere else.
Put the check and the change together in the aggregate method.

How the store backs each invariant - constraints, keys, defaults - is
`modelling-domain-invariants`; use it if installed, otherwise every rule the code
relies on gets a store-level constraint where the store can express it.

## Commands, events and their owners

| | Command | Event |
| --- | --- | --- |
| Tense | Imperative: `ReserveStock` | Past: `StockReserved` |
| Meaning | A request; the owner may refuse it | A fact; it already happened |
| Sender and receiver | Any sender, exactly one handling module | Exactly one emitting module, any consumers |

- **Each event type has exactly one emitting module.** Two publishers of
  `OrderConfirmed` means two sources of one fact, and "whichever arrives first
  wins" decides the business outcome by timing.
- **An event names what happened, not what to do next.** `SendTicketEmail`
  published by orders is a command wearing an event's name, and couples the
  emitter to its consumer. Publish `OrderConfirmed`; notifications decides.
- **Publication is atomic with the state change.** Write the event to an outbox
  table in the same transaction as the aggregate, and relay it after commit.
  "Commit, then publish" loses the event on a crash between the two; "publish,
  then commit" announces a fact that may roll back. An outbox needs no broker: a
  table and an in-process poller (`FOR UPDATE SKIP LOCKED`) in the same database
  is enough for a modulith.
- **Consumers are idempotent**, because the relay delivers at least once. Use
  `designing-idempotent-boundaries` if installed; otherwise dedupe on the event
  id, recorded in the same transaction as the consumer's effect.
- **Every event carries a type and a version from the first one**, defined in
  one schema artifact and decoded at the consumer. Rows in an outbox outlive the
  deploy that wrote them, so the compiler does not check what a consumer reads.
  For changing a published shape, use `evolving-shared-contracts` if installed;
  otherwise add fields, never repurpose one, and keep old versions decodable.

## Workflows across modules are explicit

A workflow that crosses modules is a process manager (saga): its own persisted
state, reacting to events and issuing commands, with a timeout and a
compensation for each step that can be undone. "Payment captured but the hold
expired" is a state it handles (refund), not a case nobody reached. A step that
cannot be undone ends in a named state with an owner who resolves it
(`NeedsAttention`, surfaced to an operator), never in silence.

Not a chain of synchronous calls in which one module coordinates the others and
a callee calls back into the coordinator. A request handler may make **one**
synchronous command or query to another module when the caller needs the answer
now - a refused reservation goes straight back to the customer - provided it
commits in the callee's own transaction and the design states what happens if
the caller crashes after it. A handler that needs a second module is running a
workflow: its next call starts the process manager, persisting the process state
in that module's transaction. Any call after that is a step the process has
recorded, idempotent on a key the process owns, so a crash re-drives it instead
of orphaning what came before. The test is whether a crash between any two
calls leaves a recorded state that something will act on.

## The target: a systems model

Where the domain allows it, go further than DDD's defaults:

- Every piece of state has exactly one owner, and the owner changes it only in
  response to a message.
- Parts interact only through explicit ports and channels, never through shared
  state.
- Hierarchy is ownership of lifecycles - a parent starts, supervises and ends
  its children - not containment of their data.

Adopting less of it is a choice to state, with its reason, not a default.

## Rationalizations

| Excuse | Reality |
| --- | --- |
| "We're one deployable, so a shared transaction across modules is fine" | A modulith is one deployable with enforced boundaries. A shared transaction is how boundaries stop existing. |
| "No event bus - we're too small for queues" | Events are a modelling decision, not infrastructure. An outbox table and a poller in the same database are the whole bus. |
| "It's about orders, put it in orders" | Ask about lifecycle, actor and language, not topic. |
| "Pass the trx so it's all atomic" | Then the rule spans modules: redraw into one aggregate, or model the in-between state. |
| "TypeScript catches payload drift" | Not for rows written by last week's code. Version from the first event. |
| "Each call commits in its own module's transaction, so the chain follows the rules" | Separate commits stop a shared transaction, not an orphan. Crash after each call: if nothing recorded where the workflow stopped, start the process manager earlier. |
| "Versioning can be a follow-up ticket" | The first event without a version is the one you can't tell apart later. It costs one field now. |

## Common mistakes

| Symptom | Fix |
| --- | --- |
| A table's "read by" column lists another module | Expose a query or publish an event; that module keeps a read model |
| A service function takes a `tx` from a caller in another module | Commit separately; connect the two through an event or a process manager |
| `status`/`return_status` column added to another context's table | New context with its own aggregate and table |
| `publish()` called after `commit()` | Outbox row in the same transaction |
| The same event name published in two modules | Name one owner; the other emits its own fact |
| Event named in the imperative | Rename to the fact; move the decision to the consumer |
| Rules in controllers or handlers, entities with only getters | Move the check into the aggregate method that makes the change |
| A webhook handler calls into the module that called it | Webhook records its own fact; a process manager reacts |
