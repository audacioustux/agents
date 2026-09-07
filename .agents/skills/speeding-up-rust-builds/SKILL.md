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

Ask the compiler where the time went before changing anything. `cargo build --timings`
writes an HTML report to `target/cargo-timings/` showing per-unit durations, which
units each completion unblocked, and concurrency over time.

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

Split along the direction code actually depends. Cargo rejects a cycle between
packages outright — `error: cyclic package dependency`, at resolution, before
anything compiles — so a mutually-referencing module pair cannot simply be cut in
two. Untangling that coupling is the work the split depends on, not an optimisation
to do afterwards. The one legal cycle is through `dev-dependencies`, and it costs
ordering: under `cargo test` the dependency must build before the crate that tests
with it.

Give procedural macros their own crate. Cargo pipelines `rlib` dependencies — a
dependent can start once its dependency finishes metadata, before codegen — but a
proc-macro is a linkable output, so it is excluded from pipelining in both directions.
Everything depending on it waits for a full build, and a proc-macro sharing a crate
with ordinary code drags that code onto the critical path of every dependent.

Isolate code that changes often. Invalidation propagates downstream, so a
frequently-edited module sitting in a foundational crate rebuilds the world on every
save. Moving it to a leaf costs nothing and stops the cascade.

## Dev profile

Debug information is usually the largest single dev-profile cost, and the dev default
is full info. `debug = "line-tables-only"` is the cheapest setting that still gives
backtraces with file and line. `debug = 1` (`"limited"`) is a middle option — it adds
module-level info but drops type and variable info — so it is not the cheapest, and
claiming otherwise misreads the ladder. The string forms need Rust 1.71 or newer; the
numeric and boolean forms work on older toolchains.

Link-time optimisation is already effectively off in both built-in profiles: dev and
release both default to `lto = false`. Note that `false` is not `"off"` — it still
performs thin-local LTO across the crate's own codegen units, and only `"off"`
disables LTO entirely. The rule that matters is not to turn it on in dev.

Linking is often a bigger share of an incremental rebuild than compilation, because
it happens after every change and does not benefit from caching. Before configuring a
linker, check whether you already have a fast one: since Rust 1.90, `rust-lld` is the
default on `x86_64-unknown-linux-gnu`, so adding `-fuse-ld=lld` there is stale advice
and the flag can conflict with what rustc already passes. On other targets, or for
mold, the linker is still set through `.cargo/config.toml` rustflags.

Optimising dependencies while leaving your own crate unoptimised is a real lever:
`[profile.dev.package."*"]` applies to every non-workspace dependency. Prefer
`opt-level = 1` over 2 or 3 — at 2 and above a crate stops sharing monomorphised
generics across crate boundaries, which can cost more than the optimisation gains on
generic-heavy trees.

An alternative codegen backend such as Cranelift can compile substantially faster at
the cost of slower generated code, which is the right trade for dev and wrong for
release. It is nightly-only: `codegen-backend` is an unstable Cargo feature and
requires opting in explicitly, so it is not available to a project on stable.

## Guarding the result

Compile time regresses the same way any other budget does — gradually, through changes
that each seem small. A dependency added for one function, a proc-macro pulled into a
foundational crate, a generic instantiated across many types.

If build time matters enough to fix, it matters enough to measure again later. The
timing data the compiler already emits is enough; the point is looking at it on a
schedule rather than once.

## Review checklist

- Has the build been timed with `--timings`, and the critical path read rather than the total?
- Are clean and incremental builds distinguished?
- Is the intended split along a one-way dependency, or does the code still reference back?
- Do proc-macros live in their own crate, given they cannot be pipelined?
- Does frequently-edited code sit in a foundational crate?
- Is dev debug info reduced, and LTO left off?
- On x86_64 Linux, is the toolchain new enough that lld is already the default?
- If dependencies are optimised in dev, is `opt-level = 1` used rather than 2 or 3?

## Anti-patterns

- Optimising a crate that was never on the critical path.
- Planning a split across a mutually-referencing module pair, which Cargo will reject.
- LTO enabled in a dev profile.
- Full debug info where line tables would do.
- Adding `-fuse-ld=lld` on a target where rust-lld is already the default.
- `opt-level = 3` on dependencies, losing shared monomorphised generics.
- Assuming Cranelift is available on stable.
