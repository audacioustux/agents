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
backtraces with file and line. Measured on the same 33-binary relink: `debug = true`
29.2s, `line-tables-only` 10.1s, `debug = 0` 6.6s. Backtrace quality between the first
two was identical — 41 resolved frames each, against 0 with `debug = 0` — so the 2.9x
is bought by dropping variable inspection in a debugger, not backtraces.

That makes the tradeoff a per-session one rather than a project-wide loss:
`CARGO_PROFILE_DEV_DEBUG=true cargo build` restores full info for a `gdb` or `lldb`
session. Leave `[profile.bench]` on full debug regardless — profilers need it, and
bench builds are not on the edit-compile loop.

`debug = 1` (`"limited"`) is a middle option — it adds module-level info but drops
type and variable info — so it is not the cheapest, and claiming otherwise misreads
the ladder. The string forms need Rust 1.71 or newer; the numeric and boolean forms
work on older toolchains.

Link-time optimisation is already effectively off in both built-in profiles: dev and
release both default to `lto = false`. Note that `false` is not `"off"` — it still
performs thin-local LTO across the crate's own codegen units, and only `"off"`
disables LTO entirely. The rule that matters is not to turn it on in dev.

Linking is often a bigger share of an incremental rebuild than compilation, because
it happens after every change and does not benefit from caching. Check what you
actually link with before configuring anything: `rustc --print link-args` shows the
driver and its flags. Since Rust 1.90 `rust-lld` is the default on
`x86_64-unknown-linux-gnu` only, and only in the toolchains rustup distributes — a
distro-built rustc can ship with it off. On `aarch64-unknown-linux-gnu` the same
toolchain still invokes plain `cc`, so "lld is already on" is a fact about a target
and a build of the compiler, not about a version number.

Enabling it elsewhere is a nightly affair. `-Clinker-features=+lld` is unstable on
every target, including x86_64, and errors without `-Zunstable-options`; only the
`-lld` opt-out is stable, and only on x86_64. Pair it with
`-Clink-self-contained=+linker` to use the `rust-lld` that ships with the toolchain,
which is why this needs no system linker installed — and pin a dated toolchain, since
unstable flags can be renamed.

Going the other way, the supported opt-out is `-Clinker-features=-lld`. A
`-Clink-arg=-fuse-ld=...` override does work — rustc passes user link args last, so
yours wins — but it is the unsanctioned path, and rustc's own fallback for old GCC
strips only the exact string `-fuse-ld=lld`, so a differently-spelled override can
survive a retry that was meant to drop it.

If you enable lld, cap its threads in the same change. This is the part usually
omitted and it inverts the result. lld defaults to one thread per core *per link*, and
cargo already links binaries concurrently, so a workspace linking dozens of test
binaries oversubscribes the machine by the product of the two. Measured on a 12-core
box relinking 33 test binaries after a one-line change: GNU bfd 38.3s, lld with
`--threads=1` 13.1s, lld with default threads 120-150s. The fast linker was four times
slower than the one it replaced until its internal parallelism was capped —
`-Clink-arg=-Wl,--threads=1` leaves the parallelism where cargo can schedule it.

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

## Detection patterns

| Symptom | Fix |
| --- | --- |
| A crate was sped up and total build time did not move | It was never on the critical path; read `--timings` |
| Split plan blocked by a mutual reference | Untangle the direction first; Cargo rejects package cycles outright |
| Proc-macro sharing a crate with ordinary code | Give it its own crate; it cannot be pipelined |
| A frequently-edited module in a foundational crate | Move it to a leaf; invalidation flows downstream |
| `debug = true` in a dev profile | `line-tables-only` keeps backtraces and cuts ~3x off relinking |
| `[profile.bench]` stripped of debug info | Restore it; profilers need it, and bench is off the edit loop |
| `lto` enabled in dev | Turn it off; it optimises runtime nobody is measuring mid-loop |
| lld enabled, links got slower | Cap its threads: it spawns one per core *per link* |
| `-fuse-ld=lld` added to opt out of lld | Use `-Clinker-features=-lld`, the supported opt-out |
| `.cargo/config.toml` sets a linker nobody confirmed is in use | `rustc --print link-args`; the default is per target and per distribution |
| `opt-level = 3` on dependencies in dev | Use `1`; 2+ stops sharing monomorphised generics across crates |
| Cranelift configured on a stable toolchain | It is nightly-only |

## Review checklist

- Has the build been timed with `--timings`, and the critical path read rather than the total?
- Are clean and incremental builds distinguished?
- Is the intended split along a one-way dependency, or does the code still reference back?
- Do proc-macros live in their own crate, given they cannot be pipelined?
- Does frequently-edited code sit in a foundational crate?
- Is dev debug info reduced, and LTO left off?
- Has `--print link-args` confirmed which linker actually runs, per target rather than per toolchain?
- If lld is enabled, are its threads capped so concurrent links do not oversubscribe?
- Does `[profile.bench]` still carry full debug info for profilers?
- If dependencies are optimised in dev, is `opt-level = 1` used rather than 2 or 3?

## Anti-patterns

- Optimising a crate that was never on the critical path.
- Planning a split across a mutually-referencing module pair, which Cargo will reject.
- LTO enabled in a dev profile.
- Full debug info where line tables would do.
- Using a `-fuse-ld` override to turn lld off: the supported opt-out is `-Clinker-features=-lld`.
- Enabling lld without capping its threads, making links slower than the linker replaced.
- Assuming a linker default is toolchain-wide when it landed for one target and one distribution.
- Stripping debug info from `[profile.bench]`, where profilers need it.
- `opt-level = 3` on dependencies, losing shared monomorphised generics.
- Assuming Cranelift is available on stable.
