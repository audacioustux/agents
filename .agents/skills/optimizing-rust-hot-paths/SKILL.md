---
name: optimizing-rust-hot-paths
description: Use when Rust code runs per request, per item, or per message and its CPU, allocation, or memory cost matters — sizing collections, reusing buffers, choosing a hasher, shrinking an often-instantiated type, batching work, buffering I/O, or deciding whether an optimisation or a release-profile setting is worth it before measuring.
uses:
  - name: writing-idiomatic-rust
    source: audacioustux/agents
  - name: handling-rust-errors
    source: audacioustux/agents
  - name: reviewing-unsafe-rust
    source: audacioustux/agents
  - name: writing-async-rust
    source: audacioustux/agents
  - name: speeding-up-rust-builds
    source: audacioustux/agents
---

# Optimizing Rust Hot Paths

## Overview

Rust is fast by default, so most of a hot path's cost is not in the language. It is in
what the source does not show: an allocation per call, a copy of a large value, a
hash that is stronger than the keys need, a pointer chased through three layers of
`Arc`.

Measure, then remove the silly slowdowns before attempting clever speedups. The
biggest wins usually come from data structures and algorithms. Small ones add up when
there are enough of them.

## When to use

Use for:

- code that runs per request, per row, per message, or in an inner loop
- a type instantiated thousands of times or more
- an API whose shape forces callers to allocate
- a release-profile or allocator change someone wants to make

Compile time belongs to `speeding-up-rust-builds`. Unchecked indexing and other
`unsafe` speedups belong to `reviewing-unsafe-rust`. The size of futures belongs to
`writing-async-rust`.

**Calibrate.** On a hot path that has been identified, each item here is a finding,
except that an item gated on a measurement (a hasher swap, a small-vector type, a
release-profile setting) stays gated: on a hot path you take the measurement; you do
not skip it. Elsewhere, prefer the clearer code — with one exception. Clippy's `perf`
group and the cheap habits (sizing a collection whose length you already know, not
cloning to read) cost nothing in clarity, so apply them everywhere.

## Measure first

Decide early whether the crate is performance-relevant. If it is, name its hot paths,
put benchmarks around them, and profile regularly. Retrofitting a benchmark after a
regression means you no longer know the baseline.

- **Benchmarks:** `criterion` or `divan` for micro-benchmarks, `hyperfine` for whole
  programs. Wall time is noisy; tiny memory-layout changes move it. Instruction or
  cycle counts vary less, so prefer them when comparing small changes.
- **Profiles:** a CPU profiler for time, and a heap profiler (DHAT, or a counting
  global allocator in a test) for allocations. If `malloc` and `free` are hot in a CPU
  profile, allocation rate is the problem. Keep debug info in the profile you measure
  (`[profile.bench] debug = 1`), or the profile cannot name functions.
- **Guard what you won.** A static size assertion on a hot type
  (`const _: () = assert!(size_of::<Hot>() <= 64);`, under the target architecture
  you measured) and a test asserting an allocation count stop a regression at review
  time rather than in production.

A comment beside optimised code should cite the measurement that justifies it: "99%
of these vectors hold 0 or 1 element, so those cases come first". Without it, the next
reader simplifies the code back.

## Allocation

**Size collections you can size.** When the final or approximate length is known, use
`with_capacity` (or `reserve`) rather than growing from empty; a `Vec` grows through
4, 8, 16, 32, reallocating and copying each time. `collect` and `extend` do this
themselves from the iterator's `size_hint`, so prefer them to a `push` loop.

**Reuse rather than reallocate.** Keep a "workhorse" buffer outside the loop and
`clear()` it each pass; `clear` keeps the capacity. Reading lines with `read_line`
into one `String` allocates a handful of times; `lines()` allocates once per line.
`a.clone_from(&b)` reuses `a`'s allocation where `a = b.clone()` would not.

**Let callers reuse, too.** A hot API that returns a fresh `Vec` or `String` forces an
allocation per call. Offer a form that fills a caller-owned buffer —
`fn read_into(&mut self, buf: &mut Vec<u8>)` — and keep the allocating form as the
convenience. This does not contradict the API Guidelines' "no out-parameters" rule.
That rule is about returning several values, and it names buffer reuse as its
exception.

**Strings allocate quietly.** `format!`, `to_string`, and `to_owned` each allocate.
Write into an existing buffer with `write!`, keep `&'static str` where the text is
fixed, and use `Cow<'static, str>` where most values are literals and a few are built.
To join pieces on a hot path, size once and append: `String::with_capacity(total)`
then `push_str`, or `[a, b, c].concat()`. `format!` and `write!` go through
`core::fmt`'s dynamically dispatched machinery; they are the readable choice off the
hot path, not the fast one on it.

**Small collections.** If most vectors hold a few elements, `SmallVec` keeps them
inline and allocates only past `N`. It is slightly slower per operation than `Vec`, and
larger by value, so measure. When the maximum length is known exactly, `ArrayVec`
drops the heap fallback altogether.

## Footprint

Shrinking an often-instantiated type puts more of them in cache and moves fewer bytes.

- Measure with `size_of`, or `RUSTFLAGS=-Zprint-type-sizes` on nightly, which shows
  each variant and its padding.
- An enum is at least as large as its largest variant. Box the payload of a large,
  rare variant.
- Store indices as `u32` or smaller where the range allows, and widen at use.
- A frozen sequence in a type with many instances can be `Box<[T]>` or `Box<str>`
  rather than `Vec<T>` or `String`. That is two words instead of three, and no spare
  capacity.
- Call `shrink_to_fit` on a long-lived collection built without an exact size; growth
  can leave it at nearly twice the space it needs.
- Types over 128 bytes are copied with `memcpy` rather than inline moves. If
  `memcpy` shows in a profile, shrink the type, or pass it by reference.

## Indirection

Every pointer hop on a hot path is a potential cache miss, and hops through nested
`Arc`s are sequential: the second address is unknown until the first load returns.
Embed data rather than sharing it, and lift a hot field (an `enabled` flag, a length)
into the struct that is read most. The ownership rule behind this — one owner, `Arc`
only for genuine cross-thread sharing — is `writing-idiomatic-rust` § Shared
ownership.

## Hashing

Std's default hasher (SipHash 1-3 today) resists collision attacks, and pays for it:
it is slow on short keys such as integers.

**Default, overridable — keep the default hasher for keys an attacker can
influence; use a vetted fast hasher for trusted internal keys once hashing shows in a
profile.** Options include `rustc-hash` (`FxHashMap`, very fast on integers),
`foldhash`, and `ahash`. Results differ by workload. In rustc, moving from FxHash back
to the default cost 4–84%, while moving from FxHash to `ahash` cost 1–4%, so benchmark
more than one. Prefer
a maintained crate to a hand-written `Hasher`: weak mixing in the low bits, which
hashbrown uses to pick buckets, is easy to get wrong and hard to notice. Once you
choose, forbid the default in that crate with Clippy's `disallowed_types`, so it does
not creep back.

Keys that are already uniformly random (generated ids) may not need hashing at all;
`nohash-hasher` passes them through. That is only safe while the keys stay random.

## Throughput

Optimise for items per CPU cycle, not just for latency.

- Offer and use batched operations. Per-item calls pay setup, locking, and dispatch
  per item.
- Partition work ahead of time, and let each thread or task own its slice. Shared
  state is worth it only when sharing costs less than recomputing.
- **Default, overridable — do not spin to receive single items sooner;** sleep or
  yield when there is no work. Override it for a core dedicated to a busy-poll loop
  (io_uring with SQPOLL, DPDK-style polling), where spinning is the design and the core
  does nothing else.
- A long CPU-bound async loop needs yield points; `writing-async-rust` covers the
  budget.

## I/O

File and socket I/O is unbuffered by default. Many small `write`s are many system
calls. Wrap the handle in `BufWriter` or `BufReader`, and call `flush()` explicitly at
the end: the implicit flush on drop swallows its error. `println!` locks stdout on
every call, so lock it once (`stdout().lock()`) for a burst of writes. To process
bytes rather than text, `read_until` skips UTF-8 validation.

## Bounds checks

Bounds checks cost less than people assume, and the optimiser removes many of them.
Before reaching for anything unsafe: iterate instead of indexing; slice once before
the loop (`let s = &v[..n];`) so the compiler knows the length; or `assert!` the range
up front so later checks fold away. `get_unchecked` is a last resort, taken after a
measurement, under `reviewing-unsafe-rust`.

## Telemetry on the hot path

Assume logging is enabled in production, under load. An inner loop should emit nothing
per item. Where it must, emit fields rather than a `format!`-built string, and prefer
one event per batch that lets someone reconstruct the detail offline.

## Release-profile and platform settings

LTO, `codegen-units = 1`, profile-guided optimisation, `-C target-cpu=native`,
`panic = "abort"`, and a replacement allocator such as mimalloc or jemalloc can each
help a given binary, and can each cost build time, portability, or behaviour.

**Default, overridable — none of these until a benchmark of the real workload shows
the gain, one change at a time.** Microsoft's guidelines make mimalloc and the highest
`target-cpu` the deployment fleet guarantees (such as `x86-64-v3`) defaults for
applications. They are not defaults here, because each depends on facts a guideline
cannot know:

- an allocator's effect depends on the program's allocation pattern;
- a CPU level is safe only if every machine that will ever run the binary has it, and
  `native` means only the build machine;
- `panic = "abort"` removes the option of catching a panic (see
  `handling-rust-errors`).

Libraries set none of them; the final binary owns its profile.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| Optimisation with no benchmark or profile behind it | Measure first; keep it only if it shows |
| `Vec::new()` then a `push` loop of known length | `with_capacity(n)`, or `collect` |
| New `Vec`/`String` allocated inside a hot loop | Hoist one buffer; `clear()` each pass |
| Hot API returning a fresh `Vec` per call | Add a `_into(&mut buf)` form |
| `for line in reader.lines()` on a hot path | `read_line` into one reused `String` |
| `a = b.clone()` where `a` already has capacity | `a.clone_from(&b)` |
| `format!` or `to_string` per item in a loop | `push_str` (or `write!`) into a reused buffer, or a literal |
| `s1 + &s2 + &s3 + &s4` on a hot path | `String::with_capacity` plus `push_str`, or `[..].concat()` |
| Enum whose size is set by one rare large variant | Box that variant's payload |
| `Vec<String>` frozen after build, in a type with many instances | `Box<[Box<str>]>` |
| Long-lived collection grown from empty | `shrink_to_fit` after building |
| `Arc` chain read on every request | Embed the data; lift the hot field |
| Default `HashMap` with internal integer keys, hashing hot in the profile | `FxHashMap` or `foldhash`, benchmarked |
| Hand-written `Hasher` on a hot map | A maintained hasher crate |
| Fast hasher on keys a client controls | Default hasher, or a keyed one |
| Per-item call into a batched backend | A batch API |
| Unbuffered `File` with many small writes | `BufWriter`, plus an explicit `flush()` |
| `println!` in a loop | Lock stdout once, and buffer |
| `get_unchecked` without a measurement | Iterate, slice before the loop, or assert the range |
| Per-item log line in an inner loop | One structured event per batch |
| `target-cpu=native` or a new allocator with no benchmark | Revert until measured on the target |

## Review checklist

- Is this path hot by measurement, and is there a benchmark guarding it?
- Does every collection whose length is known get it up front?
- Does anything allocate per item that could reuse a buffer?
- Can callers of a hot API avoid its allocation?
- Is any often-instantiated type larger than it needs to be?
- Is the hasher chosen for the keys' trust level, and backed by a measurement?
- Is work batched where the backend allows it?
- Is I/O buffered, with errors from the final flush observed?
- Was each release-profile or allocator change measured on its own?

## Anti-patterns

- Optimising without a profile, or keeping an optimisation that did not measure.
- `unsafe` to remove a bounds check the optimiser already removed.
- Pre-sizing everything to a guess, then never shrinking it.
- A fast non-keyed hasher on attacker-controlled keys.
- Turning on LTO, `native`, and a new allocator together, so no one knows which helped.

## Sources

Verified against these on 2026-11-05:

- The Rust Performance Book (Nicholas Nethercote): Benchmarking, Build Configuration, Linting, Profiling, Hashing, Heap Allocations, Type Sizes, Iterators, Bounds Checks, I/O, General Tips.
- Microsoft Pragmatic Rust Guidelines: M-HOTPATH, M-THROUGHPUT, M-MEM-REUSE, M-INITIAL-CAPACITY, M-SHRINK-TO-FIT, M-BOX-DST, M-AVOID-INDIRECTION, M-FAST-HASHER, M-LOG-OVERHEAD, M-MIMALLOC-APPS, M-TARGET-CPU.
- Rust API Guidelines: C-NO-OUT, C-CALLER-CONTROL.
- Google Rust Style Guide: "Prefer `format!` / `write!` for concatenation and substitution" (adopted for readability off the hot path only).
