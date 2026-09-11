---
name: handling-money-arithmetic
description: Use when code stores, compares, splits, rounds, or reconciles monetary amounts — pricing, fees, discounts, tax, payouts, refunds, invoices, or any ledger. Also when two systems disagree about a balance, or a total is off by a cent nobody can account for.
uses:
  - name: designing-idempotent-boundaries
    source: audacioustux/agents
---

# Handling Money Arithmetic

Money bugs are quiet. A rounding error does not raise; it accumulates, and it is
found weeks later by someone reconciling two systems that disagree by an amount
nobody can explain.

## Store integers in the smallest currency unit

A monetary amount is a count of indivisible units — cents, pence, satoshi — not a
quantity of currency. Store it as an integer of those units and never as a float
or double.

Binary floating point cannot represent most decimal fractions. `0.1 + 0.2` is not
`0.3`, and the discrepancy is not a display artefact you can format away: it is
in the stored value, it compounds under summation, and it makes equality
comparison unreliable. A ledger whose rows are floats cannot be proven to balance,
because the sum of the parts depends on the order you added them.

Guard the type at the boundary of the system, not in the middle. A float that
reaches the database has already lost precision, and no amount of care downstream
recovers it. That includes:

- Columns typed as float or double — a schema defect, regardless of what the
  application does.
- Serialisation formats that parse numbers as double by default, which is most
  JSON parsers. Amounts crossing that boundary belong in a string or an integer
  field.
- Summing in a language that has one numeric type and it is a float.

Where a language offers an arbitrary-precision decimal, it is correct but not
sufficient — it removes the representation error while leaving every rounding
decision below still to be made.

## Round once, at the level the customer sees

The hard part is not rounding. It is that rounding is not distributive: rounding
each component and summing gives a different answer from summing and rounding
once, and both are defensible until you pick one and apply it everywhere.

The rule that survives audit:

**Round once, at the level of the amount the customer is charged. Then derive the
components from that total so they sum to it exactly.**

If a total of 100 splits three ways, the parts are not "33.33 each" — that is
99.99 and a cent has vanished. They are 33.34, 33.33, 33.33, or whatever
allocation rule you choose, but they **sum to the rounded total by
construction**. Allocate the remainder deterministically: largest-share-first,
first-listed-first, anything stated and stable. What matters is that the rule is
written down and the same on every run, because a non-deterministic allocation
makes two runs of the same report disagree.

Rounding direction must also be stated. Half-up, half-even, and truncation differ
systematically — truncation biases against the payee on every transaction, which
is a small amount per row and a large amount per year.

Beware of rounding a value that was computed as a float quotient. A product that
should land exactly on a half can sit one unit in the last place below it, so
round-half-up rounds it down — an off-by-one-cent that appears only for specific
inputs and is nearly impossible to reproduce from a bug report. Compute the
quotient in integer arithmetic: multiply first, divide last, and take the
remainder explicitly. If the only available division returns a float, the
rounding step is unsafe and the calculation has to be restructured, not rounded
more carefully.

## Derive one side, assert the other

Every money flow has an invariant connecting its parts. Write it down as an
assertion the system checks, not as a comment.

- The sum of allocated components equals the rounded total.
- Gross minus fees minus refunds equals the payout.
- For every credit there is an equal and opposite debit.

The last one is double-entry. Use it in any system that moves money between
parties. Its value is not bookkeeping tradition: it is that
a single-entry ledger can be wrong in a way no query detects, while a double-entry
ledger that does not balance is detectably wrong by a query you can run on a
schedule.

Check the invariant where it can fail, which is the write path, and check it again
on a sweep. A reconciliation sweep that runs nightly and asserts the books balance
converts a class of silent corruption into an alert.

**The most common way a ledger goes wrong is not arithmetic — it is the same
event applied twice.** A payment confirmation redelivered, a refund submitted on a
double-click, a payout job re-run after a timeout: each produces a second entry
that is individually correct and collectively wrong. No rounding rule protects
against it, and the invariant above will report the imbalance without explaining
it. Every handler that moves money needs a deduplication key it did not mint
itself — see `designing-idempotent-boundaries`.

## Reconciling against a system you do not control

When an external party also holds a record of the same money, your ledger and
theirs will diverge. Design for that as a normal condition rather than an
incident:

- **Their record is authoritative for what happened; yours is authoritative for
  what it means.** They know whether a charge settled. You know which order it
  belonged to.
- **Divergence needs a resolution rule decided in advance**, because deciding it
  during an incident means deciding it under pressure with money at stake. Write
  down which side wins for each class of disagreement, and what a human is
  expected to do with the ones where neither does.
- **A reconciliation that reports "N mismatches" and nothing else is not
  actionable.** It must say which records, in which direction, by how much, and
  which side it believes.

## Amounts are not comparable across currencies

An integer count of minor units is meaningless without the currency it counts.
Store them together and treat the pair as the value; a bare integer that has been
separated from its currency will eventually be added to one from another.

Minor-unit scale is not universally two. Some currencies have zero decimal places
and some have three, so a hard-coded factor of 100 is a latent bug in any system
that adds a currency later.

## Review checklist

- Is any monetary value stored, parsed, or summed as a float or double?
- Is there exactly one rounding step, and is its direction stated?
- Do allocated components provably sum to their total, with a deterministic rule
  for the remainder?
- Is there an invariant that a query could check, and does anything check it?
- When an external record disagrees, is the resolution rule written down?
- Is every amount stored with its currency, and is the minor-unit scale derived
  rather than assumed?
- Can any money-moving handler be invoked twice for the same event, and what stops
  the second invocation from writing a second entry?
