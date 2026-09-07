---
name: writing-idiomatic-rust
description: Use when writing or reviewing Rust — choosing between borrowing and owning at an API boundary, handling fallible operations, picking static or dynamic dispatch, or deciding whether a clone is paying for itself.
uses:
  - name: reviewing-unsafe-rust
    source: audacioustux/agents
  - name: speeding-up-rust-builds
    source: audacioustux/agents
  - name: writing-async-rust
    source: audacioustux/agents
---

# Writing Idiomatic Rust

## Overview

Most Rust review findings are one of three things: a signature that takes more
ownership than it needs, a fallible path that panics instead of returning, or a
`clone` standing in for a decision nobody made.

The compiler already enforces memory safety. What it cannot tell you is whether the
ownership design you gave it is the one you meant.

## When to use

Use for:

- new Rust code, and review of existing Rust
- a signature decision: owned or borrowed, generic or `dyn`
- error plumbing: what returns `Result`, what may panic
- a `clone` you are not sure is necessary

Defer to `reviewing-unsafe-rust` for anything inside an `unsafe` block or crossing an
FFI boundary, to `writing-async-rust` for await points and cancellation, and to
`speeding-up-rust-builds` for compile-time cost.

## Ownership at the boundary

Take the least owned thing that does the job. A parameter typed `&str` accepts a
string literal and, by deref coercion, a `&String` — so a caller holding a `String`
passes `&s` and keeps it, while a parameter typed `String` forces that caller to give
it up or clone. The same holds for `&[T]` against `Vec<T>`. Widening a parameter this
way costs nothing and removes an allocation from callers who had a borrow already.

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

## What a clone is telling you

A `clone` is not a defect. A `clone` you cannot justify in one sentence is.

Before writing one, name which of these it is: the callee genuinely needs its own
copy that outlives the caller's; the type is `Copy`-cheap and cloning is clearer than
threading a lifetime; or the borrow checker rejected the design and cloning made the
error go away. Only the third is a problem, and it is a problem because the error was
information about the design and the clone discarded it.

Cloning inside a loop is worth a second look on sight. It is rarely the intended cost
and usually indicates the loop is holding a borrow it could have taken once outside.

## Fallibility

Return `Result` for anything a caller could reasonably encounter and handle. Reserve
panics for states that mean the program's own invariants are broken, where continuing
would be worse than stopping.

`unwrap` and `expect` are assertions that a case cannot happen. In tests that is
exactly right, and the panic message is the failure report. In library and application
code they are a claim about reachability that nothing checks. Where the case truly
cannot happen, `expect` with a message stating *why* is better than `unwrap`, because
the message is the proof and it survives into the panic output.

Propagate with `?` rather than a match chain whose only job is to return the error
unchanged. The chain adds lines without adding a decision, and it buries the one arm
that does something.

`let ... else` handles the shape where a binding must succeed or the function returns:
it keeps the happy path unindented and puts the failure beside it, which nested
matching does not.

Distinguish the placeholder macros rather than reaching for whichever comes to mind.
`todo!` marks work not yet written, `unimplemented!` marks a case deliberately not
supported, and `unreachable!` asserts a state the logic excludes. They read
identically at runtime and completely differently to the next person.

## Designing the error type

A library and a binary want opposite things from an error, which is why one choice
cannot serve both.

A library's caller needs to branch on what went wrong, so its error should be an enum
with a variant per failure mode the caller could handle differently. That gives them
a `match` and lets the compiler tell them when a new variant arrives. Deriving the
boilerplate is fine; the design decision is the variant set, not the derive.

A binary's "caller" is a human reading a message, so it needs context on the way out
rather than branchable structure. A single opaque error carrying a chain of context
is the right shape there, and it is the wrong shape in a library precisely because it
erases the distinctions a caller would have branched on.

That is the whole rule: a boxed or opaque error in a library forces every caller to
string-match or give up. Reserve it for the top of an application, where nothing
downstream needs to tell the cases apart.

Preserve the underlying cause when wrapping. An error that discards what it wrapped
turns a diagnosable failure into a guess, and the source chain is what makes a
message useful three layers up.

Errors that cross an async or thread boundary need to be sendable and self-contained,
which in practice means not borrowing from the frame that produced them. This is
easier to design in at the start than to retrofit once a type has callers.

## Iterators

Prefer an iterator chain to a manual index loop. The bounds check the loop needs is
one the iterator does not, and the intent survives the next edit better than a
counter does.

Avoid materialising an intermediate collection between two adapters. A `collect`
whose result is immediately iterated again has allocated a vector to hold values that
were already in flight.

One trap deserves naming: `iter()` on a collection of `Copy` types yields references,
and `into_iter()` yields values. Chaining an adapter that expects values onto an
`iter()` produces a type error, and the reflexive fix is to insert a `cloned()` or
`copied()` that would not have been needed had the right method been called. Choose
the iterator by what the consumer wants, not by which one compiles first.

## Dispatch

Generics resolve at compile time, so each instantiation is specialised and inlinable,
at the cost of one copy of the code per type. Trait objects resolve at run time
through a vtable, so there is one copy of the code and one indirection per call.

Reach for generics on hot paths and for anything the optimiser should see through.
Reach for `dyn Trait` when the set of types is open, when a collection must hold
several of them, or when monomorphising a large function across many types would cost
more compile time than the indirection costs at run time.

Where a boxed trait object is the right answer, put it at the API boundary rather
than threading it through internals. The boundary is where the openness is real; the
internals usually know their type.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| `fn f(s: String)` where the body only reads | Take `&str`; callers with a `String` pass `&s` and keep it |
| `.clone()` added right after a borrow-checker error | Revisit the ownership design; the error was information |
| `.clone()` inside a loop body | Hoist the borrow outside the loop |
| `.unwrap()` in library or application code | Return `Result`, or `.expect("why this cannot happen")` |
| `match` whose only arm returns the error unchanged | Replace with `?` |
| Nested `match` to bind one value or return | `let ... else` |
| `.collect()` immediately followed by iteration | Drop the intermediate; chain the adapters |
| `.iter()` then `.cloned()` on a `Copy` collection | `.into_iter()`, or `.copied()` if the borrow is wanted |
| Library error is `Box<dyn Error>` or a single opaque type | Enum with a variant per failure the caller can handle |
| Wrapper error discards what it wrapped | Keep the cause; implement `source()` |
| `dyn Trait` inside a module that knows the concrete type | Generic here; box at the API boundary instead |

## Review checklist

- Does any parameter take ownership the function does not keep?
- Can each `clone` be justified in one sentence?
- Does any `clone` sit inside a loop?
- Is any `unwrap` outside a test asserting something nothing verifies?
- Does any match chain exist only to return its error unchanged?
- Does a library expose an opaque error where callers need to branch?
- Does any wrap discard the cause it wrapped?
- Is an intermediate collection allocated between two adapters?
- Is a generic used where the type set is genuinely open, or `dyn` on a hot path?

## Anti-patterns

- Cloning to silence the borrow checker, then keeping the clone.
- `String` and `Vec<T>` parameters on functions that only read.
- `unwrap` in library code, with the reachability argument left in someone's head.
- A match chain wrapping a single fallible call.
- An opaque or boxed error in a library, forcing callers to string-match.
- `iter()` plus `cloned()` where `into_iter()` was meant.
- Boxing a trait object inside a module that knows the concrete type.
