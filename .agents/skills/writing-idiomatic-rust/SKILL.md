---
name: writing-idiomatic-rust
description: Use when writing or reviewing Rust — choosing between borrowing and owning at an API boundary, reaching for `Rc` or `Arc`, deciding whether a clone is paying for itself, converting between integer types, choosing an iterator chain or a loop, or picking static or dynamic dispatch.
uses:
  - name: handling-rust-errors
    source: audacioustux/agents
  - name: designing-rust-apis
    source: audacioustux/agents
  - name: optimizing-rust-hot-paths
    source: audacioustux/agents
  - name: configuring-rust-lints
    source: audacioustux/agents
  - name: reviewing-unsafe-rust
    source: audacioustux/agents
  - name: speeding-up-rust-builds
    source: audacioustux/agents
  - name: writing-async-rust
    source: audacioustux/agents
---

# Writing Idiomatic Rust

## Overview

Most Rust review findings are a signature that takes more ownership than it needs,
shared ownership standing in for a decision about who owns what, or a `clone`
standing in for a decision nobody made.

The compiler already enforces memory safety. What it cannot tell you is whether the
ownership design you gave it is the one you meant.

## When to use

Use for:

- new Rust code, and review of existing Rust
- a signature decision: owned or borrowed, generic or `dyn`
- `Rc`, `Arc`, or a `clone` you are not sure is necessary
- integer conversions and arithmetic that could overflow
- an iterator chain that is getting hard to read

This is the entry point for Rust work, and it hands off:

- panics, `Result`, and error types: `handling-rust-errors`
- a library's public surface — naming, traits, newtypes, `#[must_use]`,
  `#[non_exhaustive]`, builders, docs, features: `designing-rust-apis`
- allocation, capacity, hashing, and other hot-path costs: `optimizing-rust-hot-paths`
- the crate's lint configuration: `configuring-rust-lints`
- `unsafe` and FFI: `reviewing-unsafe-rust`
- await points and cancellation: `writing-async-rust`
- compile time: `speeding-up-rust-builds`

## Ownership at the boundary

Take the least owned thing that does the job. A parameter typed `&str` accepts a
string literal and, by deref coercion, a `&String` — so a caller holding a `String`
passes `&s` and keeps it, while a parameter typed `String` forces that caller to give
it up or clone. The same holds for `&[T]` against `Vec<T>`. Widening a parameter this
way costs nothing and removes an allocation from callers who had a borrow already.

Borrow the data, not its container: `&T` rather than `&Box<T>`, `&[T]` rather than
`&Vec<T>`, `&str` rather than `&String`, `Option<&T>` rather than `&Option<T>`. The
container in the signature buys nothing and rejects callers who hold the data some
other way.

The converse also holds. If the function keeps the value, take it by value and let
the caller decide whether to clone. Borrowing and then cloning inside hides an
allocation the caller might have avoided by moving.

Return owned data when the caller needs it to outlive the callee, and borrowed data
when it does not. A function returning `String` where every caller immediately reads
and drops it has allocated for nothing.

Reach for `Cow` when ownership genuinely depends on input rather than on the
signature: a normaliser that returns its argument unchanged most of the time, and an
owned value only when it had to rewrite something.

Small `Copy` types are the exception. Passing a `u64` or a two-field struct by value
is cheaper than passing a reference to it, because the reference itself is pointer
sized and adds an indirection. The threshold is a few machine words, not a
philosophy.

## Shared ownership: `Rc` and `Arc`

**Default, overridable — single ownership plus borrowing; `Rc` within one thread;
`Arc` only where data genuinely crosses threads.**

Shared ownership hides who owns what. Drop order and lifetime become run-time facts
that no signature states. `Arc` also has a cost the source does not show: every
clone and drop is an atomic read-modify-write on one shared cache line. In a
thread-per-core, shared-nothing design, a count touched from several cores bounces
that line between them. Google's guide says to prefer unique ownership and borrowing,
and Microsoft's warns against reflexively wrapping nested types in `Arc`; what that
nesting costs on each access is `optimizing-rust-hot-paths` § Indirection.

- `Rc` is acceptable inside one thread or core, when a structure really has several
  owners. It is not an escape from a borrow-checker error.
- `Arc` is for the few hand-offs between threads: a configuration snapshot, a
  read-only table built once.
- `Arc` is also fine inside a cold service handle that exists a few times per
  process and is `Clone` (Microsoft's M-SERVICES-CLONE). Keep it private, as an
  `Arc<Inner>` field, so the signatures still say `&Service`.
- Never one `Arc` per hot, high-cardinality entity — per request, per row, per
  message. Give such entities one owner (an arena, a table, a slab) and pass indices
  or borrows.
- Write `Arc::clone(&x)` rather than `x.clone()`, so a reader can tell a count bump
  from a deep copy. Clippy's `clone_on_ref_ptr` enforces this.

Override when the single-owner structure would be unreasonably complex to write or
maintain — Google's own exception — and the shared values are few and cold. Say which
case applies in the review.

## What a clone is telling you

A `clone` is not a defect. A `clone` you cannot justify in one sentence is.

Before writing one, name which of these it is: the callee genuinely needs its own
copy that outlives the caller's; the type is `Copy`-cheap and cloning is clearer than
threading a lifetime; or the borrow checker rejected the design and cloning made the
error go away. Only the third is a problem, and it is a problem because the error was
information about the design and the clone discarded it.

Cloning inside a loop is worth a second look on sight. It is rarely the intended cost
and usually indicates the loop is holding a borrow it could have taken once outside.

## Integers: overflow and casts

Integer overflow panics in a debug build and silently wraps in release, with default
profiles. Code that overflows therefore behaves differently in the two builds, and
tests run in debug never see the release behaviour. Where overflow is possible, say
which behaviour you mean: `checked_*` for an `Option`, `wrapping_*` or `Wrapping` for
modular arithmetic, `saturating_*` or `Saturating` to clamp, or a range check before
the operation.

`as` between numeric types never fails. It truncates, flips sign, or rounds
silently. Use `From`/`Into` for conversions that cannot lose information, and
`TryFrom` for those that can, so the code says which kind it is. Clippy's pedantic
cast lints (`cast_possible_truncation`, `cast_sign_loss`, `cast_lossless`) find the
rest.

Calibrate this. It is a finding where the value can come from input, or can exceed the
target range. It is a preference where the range is proven a line away. Even there,
`usize::from(x)` states the intent that `x as usize` hides.

## Iterators

Iterate rather than index. The intent survives the next edit better than a counter
does; what it saves in bounds checks is `optimizing-rust-hot-paths` § Bounds checks.

Avoid materialising an intermediate collection between two adapters. A `collect`
whose result is immediately iterated again has allocated a vector to hold values that
were already in flight.

Between an adapter chain and a `for` loop, choose by what reads more clearly.

- A chain wins when the adapters *are* the logic: `any`, `sum`, `filter_map`, `zip`.
  The equivalent loop re-implements them by hand.
- A `for` loop wins when the body branches, mutates state outside the iterator, or
  returns early with `?`. A `map` closure that increments an outside counter is a loop
  pretending to be a chain.
- Never build a long chain inside a `for` header. Bind it to a named variable first;
  the name documents what the chain selects.
- Prefer `for` to `Iterator::for_each`, unless the chain is long and a `for` would
  need it named anyway.

Annotate the type where inference hides it: the result of `.collect()`, `.parse()`,
`.into()`, or a long chain. Put it on the binding, not in a turbofish, when you have
the choice.

One trap deserves naming: `iter()` on a collection of `Copy` types yields references,
and `into_iter()` yields values. Chaining an adapter that expects values onto an
`iter()` produces a type error, and the reflexive fix is to insert a `cloned()` or
`copied()` that would not have been needed had the right method been called. Choose
the iterator by what the consumer wants, not by which one compiles first.

## Dispatch

Generics resolve at compile time, so each instantiation is specialised and inlinable,
at the cost of one copy of the code per type. Trait objects resolve at run time
through a vtable, so there is one copy of the code and one indirection per call.

**Default, overridable — a concrete type first, then generics, then `dyn` when code
size or heterogeneity demands it.** Microsoft states the same ladder. Google prefers
generics over `dyn` in libraries, except where code size matters. rust-analyzer's
style guide warns against making much code generic, especially across crate
boundaries, because all of it must be compiled.

1. **Concrete type** while one implementation exists. A trait with one implementation
   is indirection with no payoff. Define a trait for generic code, `impl Trait`, or
   `dyn Trait`, not to organise methods.
2. **A test double is not a second implementation.** Reach for sans-I/O or an internal
   enum over real and mock first; `designing-rust-apis` covers both.
3. **Generics** when callers supply their own types. Keep each trait narrow, and keep
   generics on hot paths, where the optimiser must see through the call.
4. **`dyn`** when the set of types is open at run time, when one collection holds
   several, when generic parameters start to nest through every caller, or when
   monomorphising a large function across many types costs more than the indirection.
   Wrap it in a named type at the API boundary rather than exposing `Box<dyn Trait>`
   or `Arc<dyn Trait>`; the internals usually know their type.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| `fn f(s: String)` where the body only reads | Take `&str`; callers with a `String` pass `&s` and keep it |
| `&Vec<T>`, `&String`, `&Box<T>`, `&Option<T>` parameter | `&[T]`, `&str`, `&T`, `Option<&T>` |
| `fn f(x: &T)` whose first line is `x.clone()` | Take `T` by value; let the caller decide |
| `Rc`/`Arc` added right after a borrow-checker error | Revisit ownership; give the data one owner |
| `Arc<T>` per request, row, or message on a hot path | One owner (arena, table) plus indices or borrows |
| `x.clone()` where `x` is an `Arc` | `Arc::clone(&x)` |
| `.clone()` added right after a borrow-checker error | Revisit the ownership design; the error was information |
| `.clone()` inside a loop body | Hoist the borrow outside the loop |
| `a + b` on values that can reach the type's limit | `checked_add`, `wrapping_add`, or `saturating_add`, as meant |
| `x as u32` on a value that may not fit | `u32::try_from(x)` |
| `x as u64` on a value that always fits | `u64::from(x)` |
| `.collect()` immediately followed by iteration | Drop the intermediate; chain the adapters |
| `map` closure mutating an outside counter | A `for` loop with a `match` |
| Multi-line adapter chain inside a `for` header | Bind it to a named variable first |
| `.for_each(...)` on a short chain | A `for` loop |
| `.iter()` then `.cloned()` on a `Copy` collection | `.into_iter()`, or `.copied()` if the borrow is wanted |
| Trait with exactly one implementation, not used generically | Inherent methods on the concrete type |
| `dyn Trait` inside a module that knows the concrete type | Generic or concrete here; box at the API boundary instead |

## Review checklist

- Does any parameter take ownership the function does not keep, or borrow a value it then clones?
- Is each `Rc`/`Arc` justified by real sharing, and is any `Arc` per hot entity?
- Can each `clone` be justified in one sentence?
- Can any arithmetic overflow, and does the code say what should happen if it does?
- Does any `as` cast truncate, flip sign, or round a value that can be out of range?
- Is an intermediate collection allocated between two adapters?
- Would any chain read better as a loop, or any loop as one adapter?
- Is a trait doing a concrete type's job, or `dyn` sitting on a hot path?

## Anti-patterns

- Cloning to silence the borrow checker, then keeping the clone.
- `Arc` as a default wrapper rather than a cross-thread decision.
- `String` and `Vec<T>` parameters on functions that only read.
- `as` used as a general-purpose conversion.
- A combinator chain carrying branches and side effects a loop would show plainly.
- `iter()` plus `cloned()` where `into_iter()` was meant.
- A trait introduced for a single implementation "for testability".
- An interface-per-class design ported one-to-one from C#, Java, or C++: the business
  logic may carry over, the technical constructs should not.

## Sources

Verified against these on 2026-11-05:

- Google Rust Style Guide: "Avoid `Rc` and `Arc`", "Prefer borrows over deep copies", "Accept flexible references for immutable data", "Generics versus `dyn Trait`", "When to define a trait versus an inherent implementation", "Don't overuse type inference", "Don't use iterator combinator chains when direct iteration is clearer".
- Microsoft Pragmatic Rust Guidelines: M-RUST-SHAPED, M-DI-HIERARCHY, M-SERVICES-CLONE, M-AVOID-INDIRECTION, M-AVOID-WRAPPERS.
- Rust API Guidelines: C-CALLER-CONTROL.
- rust-analyzer style guide: "Avoid Monomorphization", "Functional Combinators".
- Safety-Critical Rust Coding Guidelines (draft): integer overflow; `as` with numeric operands.
- The Rust Performance Book: Heap Allocations (`Rc`/`Arc`).
