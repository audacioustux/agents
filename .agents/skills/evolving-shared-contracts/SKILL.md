---
name: evolving-shared-contracts
description: Use when changing an interface more than one party depends on — an API response, an event payload, a message schema, a shared type, a view another service reads. Also when removing a field, when a consumer broke after a change that looked additive, or when no single artifact defines the shape.
uses:
  - name: modelling-domain-invariants
    source: audacioustux/agents
  - name: migrating-live-data
    source: audacioustux/agents
---

# Evolving Shared Contracts

A contract is any shape two parties agree on and change independently. The
defining property is not the format — it is that you cannot deploy both sides at
once, so every change has a window where old and new are both live.

## Change the contract first, then the implementations

The failure is ordinary: someone adds a field to a response, ships it, and updates
the schema afterward if at all. Now the artifact everyone reads describes
something that no longer exists, and the only accurate description of the
interface is the provider's source code — which consumers cannot see and should
not have to read.

Order the change so the agreement precedes the code:

1. State the need and which side it comes from.
2. Change the canonical artifact — the schema, the type definition, whatever both
   sides derive from.
3. Review that diff with the parties affected. The diff of the contract is the
   reviewable unit; the implementation diff is consequence.
4. Regenerate whatever is generated from it.
5. Change provider and consumers.
6. Verify from both sides.

If the contract has no canonical artifact, that is the first defect to fix.
Interfaces described in a wiki page, a README example, a mock fixture, and three
hand-written types will diverge, and each consumer will be coded against whichever
one its author happened to read.

## Additive is not automatically safe

The usual rule is "adding is safe, removing is breaking". Half true, and the
exceptions bite:

- **A consumer that validates strictly** rejects unknown fields. Adding one breaks
  it. Whether your consumers do this is a fact to establish, not assume.
- **A new required field is a breaking change to producers**, even though it looks
  additive from the consumer's side. Anything already emitting the old shape now
  emits an invalid one.
- **Repurposing a field is the worst case** — the shape is unchanged, so nothing
  flags it, and every consumer silently misinterprets the value. Widening the
  meaning of an existing field, changing its unit, or adding a variant to an
  enumeration are all this. Add a new field instead and retire the old one
  explicitly.
- **Tightening a constraint is breaking.** Narrowing a range or making an optional
  field required rejects inputs that were previously valid.

The test that actually holds: **can every currently deployed reader and writer
handle the new shape without changing?** If not, it is breaking, regardless of
whether anything was removed.

## Removal is a three-phase operation

You cannot remove a field in one deployment, for the same reason a column change
cannot be one migration — see `migrating-live-data` for the schema case.

1. **Announce and stop writing new dependencies.** The field still ships.
2. **Verify nothing reads it.** Not "grep the monorepo" — instrument the field and
   confirm zero reads over a period long enough to cover monthly and quarterly
   jobs. Consumers you do not control need a deprecation window with an actual
   date.
3. **Remove it.**

Phase 2 is the one that gets skipped, and it is the one that determines whether
phase 3 is an incident. A field you cannot prove is unread is a field you cannot
remove.

## Do not let storage shape the contract

Returning rows straight from a query makes the storage layout the public
interface. Every column rename becomes a breaking change, every new internal
column leaks, and the contract now changes for reasons that have nothing to do
with what consumers need.

Map explicitly, even when the mapping is currently identity. The cost is one
mapping function. The alternative is that the first schema refactor is a
coordinated multi-party migration. `modelling-domain-invariants` covers the
storage side of that boundary — what the schema itself must guarantee, as
distinct from what you promise the consumers of it.

This is the same failure as exposing internal enum values, database ids with
inferable ordering, or error strings that name internal components.

## Version when compatibility is genuinely impossible

Versioning is the fallback, not the first move — every version is a surface you
maintain until the last consumer leaves, which is longer than anyone plans.

Exhaust the alternatives first: a new field alongside the old, an optional
discriminator, a capability the consumer opts into. When none of those work,
version at the boundary where compatibility actually broke rather than bumping
everything, and write down what makes the old version retirable.

## Review checklist

- Is there one canonical artifact both sides derive from, or several
  descriptions that can drift?
- Did the contract change land before the implementation?
- Can every deployed reader and writer handle the new shape unchanged?
- Does any change repurpose an existing field rather than adding one?
- For a removal: is there evidence nothing reads it, and over what window?
- Does the response shape follow storage layout, or a deliberate mapping?
- If a version was added, what is the condition for retiring the old one?
