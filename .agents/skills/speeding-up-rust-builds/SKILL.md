---
name: speeding-up-rust-builds
description: Use when a Rust project's compile times are slowing the edit-test loop — measuring where the time goes, splitting crates, and tuning dev profiles without touching release output.
uses:
  - name: writing-idiomatic-rust
    source: audacioustux/agents
---

# Speeding Up Rust Builds

## Overview

Rust build time is dominated by a few structural properties of the crate graph, not
by the amount of code. A project can shed a large fraction of its incremental
rebuild time without deleting a line.

The rule that governs everything here: dev builds and release builds have different
jobs, and most of what makes a release build good makes a dev build slow.

## When to use

Use for:

- an edit-test loop that has become slow enough to change how people work
- CI build times
- deciding what belongs in its own crate

Runtime performance belongs elsewhere; this skill is about compile time only. Code
shape questions belong to `writing-idiomatic-rust`.

## Measure first

Ask the compiler where the time went before changing anything. It can report per-crate
timings and produce a timeline showing which crates blocked others.

That timeline is the thing worth reading. Total time is much less informative than
the critical path, because crates compile in parallel and only the chain that
serialises actually costs wall-clock time. Shaving a crate that was already running
alongside something slower changes nothing.

Distinguish a clean build from an incremental one. They have different bottlenecks,
and the incremental case is the one developers feel.

## The crate graph

A single crate compiles as one unit. Splitting a large one into several lets Cargo
compile them in parallel and lets an edit invalidate only part of the tree.

Three splitting rules, in order of payoff:

Break circular dependencies first. A cycle forces sequential compilation and defeats
every other split.

Give procedural macros their own crate. Everything depending on a proc-macro waits
for it, so a proc-macro sharing a crate with ordinary code drags that code onto the
critical path of every dependent.

Isolate code that changes often. Invalidation propagates downstream, so a
frequently-edited module sitting in a foundational crate rebuilds the world on every
save. Moving it to a leaf costs nothing and stops the cascade.

## Dev profile

Debug information is usually the largest single dev-profile cost, and full debug info
is more than a backtrace needs. Reducing it to line-table level typically keeps
debugging usable and removes a large fraction of build time.

Link-time optimisation belongs off in dev. It runs after compilation, over the whole
program, and it exists to improve runtime performance nobody is measuring during an
edit-test loop.

Linking is often a bigger share of an incremental rebuild than compilation, because
it happens after every change and does not benefit from caching. A faster linker is
usually the single cheapest improvement available, since it changes nothing about the
code.

An alternative codegen backend can compile substantially faster than the default at
the cost of slower generated code. That trade is right for dev and wrong for release,
so it belongs behind a dev-only profile setting rather than a global one.

Where dependencies are slow but rarely change, they can be optimised while your own
crates are not: they compile once and then sit in the cache, so paying for their
optimisation is nearly free while keeping your own iteration fast.

## Guarding the result

Compile time regresses the same way any other budget does — gradually, through changes
that each seem small. A dependency added for one function, a proc-macro pulled into a
foundational crate, a generic instantiated across many types.

If build time matters enough to fix, it matters enough to measure again later. The
timing data the compiler already emits is enough; the point is looking at it on a
schedule rather than once.

## Review checklist

- Has the build been timed, with the critical path identified rather than the total?
- Are clean and incremental builds distinguished?
- Do any dependency cycles remain?
- Do proc-macros live in their own crate?
- Does frequently-edited code sit in a foundational crate?
- Is debug info reduced in dev, and LTO off there?
- Has a faster linker been tried before restructuring code?

## Anti-patterns

- Optimising a crate that was never on the critical path.
- Splitting crates before breaking cycles.
- LTO enabled in a dev profile.
- Full debug info where line tables would do.
- Restructuring the crate graph before trying a faster linker.
- A fast-codegen backend left on for release builds.
