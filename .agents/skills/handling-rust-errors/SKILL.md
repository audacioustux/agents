---
name: handling-rust-errors
description: Use when deciding whether a Rust failure should panic, return `Option`, or return `Result`; designing a library or application error type; writing `unwrap`, `expect`, or an assertion message; handling a poisoned lock; or reaching for `catch_unwind`.
uses:
  - name: writing-idiomatic-rust
    source: audacioustux/agents
  - name: designing-rust-apis
    source: audacioustux/agents
  - name: reviewing-unsafe-rust
    source: audacioustux/agents
---

# Handling Rust Errors

## Overview

Rust has three channels for failure, and each is a promise to a different reader.
`Option` says the cause is obvious. `Result` says the caller can tell the cases apart
and act on them. A panic says the program has a bug and should stop.

Most error-handling defects send a failure down the wrong channel: an expected
failure that panics, a bug reported as an `Err` nobody can act on, or a library error
shaped for a human when its reader is code.

## When to use

Use for:

- a fallible operation: what it returns, and what may panic
- a new error type, or a change to one
- `unwrap`, `expect`, `assert!`, and their messages
- a poisoned `Mutex` or `RwLock`
- any `catch_unwind`

Ownership and API shape belong to `writing-idiomatic-rust` and `designing-rust-apis`.
A panic crossing an FFI boundary belongs to `reviewing-unsafe-rust`.

## Choosing the channel

Ask whether a correct program could hit this case. If it could, it is not a panic.

- `Option` when there is one obvious cause and nothing more to say: `checked_add`,
  a map lookup.
- `Result` when the failure comes from outside the program and correct code cannot
  rule it out in advance — I/O, parsing input, a remote call — and the caller needs
  to know which case occurred.
- A panic when the program has detected its own bug: a broken internal invariant, or
  a caller violating a documented precondition. Do not invent an error type for it.
  No caller can handle "the library is wrong" at run time, and its error conditions
  cannot be documented.

`parse(&str)` returns `Result`, because bad input is normal. A function whose contract
excludes zero may panic on zero, provided its `# Panics` doc section says so.

**Default, overridable — panic on a detected bug, never for an expected failure.**
Google and Microsoft agree on that much. They differ at the edge: Google notes that
some codebases avoid panics entirely and report even broken invariants through
`Result`. Microsoft makes panicking on a bug mandatory. The resolution is to keep the
panic and *add* a non-panicking variant where a caller cannot cheaply prove the
precondition. Std does exactly this: indexing panics and `get` returns `Option`;
`Vec::reserve` aborts on allocation failure and `try_reserve` returns `Result`.
Offer the second form for untrusted indices, embedded or kernel callers, or a
server that must not stop. Do not convert every panic into an `Err`.

Before asserting, see whether the type can carry the invariant instead. A
precondition held by a type is checked once, at construction, and cannot be
forgotten at a call site. `designing-rust-apis` covers newtypes that guard
invariants.

## Propagating

Propagate with `?` rather than a match chain whose only job is to return the error
unchanged. The chain adds lines without adding a decision, and it buries the one arm
that does something.

`let ... else` fits the case where a binding must succeed or the function returns. It
keeps the happy path unindented and puts the failure beside it.

Prefer pattern matching to check-then-unwrap. `if let Some(v) = opt` proves the case to
the compiler. `if opt.is_some() { opt.unwrap() }` proves it only to the reader, and
only until the next edit separates the check from the unwrap.

## `unwrap`, `expect`, and assertion messages

`unwrap` and `expect` assert that a case cannot happen. In tests that is exactly right,
and the panic is the failure report. Elsewhere they are claims about reachability that
nothing checks.

Prefer `expect`, with a message that says *why* the case cannot happen. The message is
the proof, and it survives into the panic output. BurntSushi's caveat holds, though:
where the line above makes the reason obvious, an `expect` message is noise, and
banning `unwrap` outright costs more than it buys. This is a judgement call, not a
lint.

An assertion message should carry what the bare condition cannot:

- why the condition should hold, or the values that broke it:
  `assert!(refund > 0, "refund must be positive, got {refund}")`
- whether failure means the caller misused the API or the implementation has a bug
- never a restatement of the condition. `"values should be sorted"` beside
  `values.is_sorted()` says nothing the expression did not.

The placeholder macros mean different things, so pick the one you mean. `todo!` marks
work not yet written, `unimplemented!` a case deliberately unsupported, and
`unreachable!` a state the logic excludes. They behave identically at run time and
read completely differently to the next person.

## A panic stops the program

A panic is not an exception. The final binary chooses `panic = "abort"` or `"unwind"`
in its profile, and a library cannot know which. So a library never relies on a panic
being caught, including by its own code. Under `abort`, a panic that "would be caught"
kills the process.

`catch_unwind` catches only unwinding panics. Std documents it as not a general
try/catch, and as not guaranteed to catch every panic.

**Default, overridable — `catch_unwind` only at an isolation boundary, chosen by
panic strategy.** Google bans it in first-party code, except as defence in depth
that is never relied upon, because its products ship with `panic = "abort"`.
Microsoft permits it per request in a server, followed by a controlled restart. Both
positions hold, under different strategies:

| Panic strategy | Rule |
| --- | --- |
| `abort` | Do not write `catch_unwind`. It catches nothing, and its presence implies a guarantee that does not exist. |
| `unwind`, library code | Do not catch. The caller owns that decision. |
| `unwind`, application boundary | Allowed where it isolates unrelated work — one request, one job, one plugin call. Discard whatever state the panicking code could have touched, and treat the panic as a bug report that triggers a restart, not a recovery. |

The danger after a caught panic is torn state. A panic between two updates that must
happen together leaves the first done and the second not, and the next reader sees a
combination the code never allowed. Anything that runs caller-supplied closures or
trait impls must at least stay memory-safe if they panic midway.

## Poisoned locks

A std `Mutex` or `RwLock` becomes poisoned when a thread panics while holding it,
because the data may now violate its invariant.

**Default, overridable — propagate the poison.** `lock().expect("cache mutex
poisoned")` turns one bug into a stop rather than letting other threads read
half-updated data. Std notes this is what most code does, and Microsoft lists it as a
legitimate panic. Google forbids reaching inside a poisoned mutex at all.

Std does allow recovery, and that is the override: overwrite the value with a
known-good one, or verify it is consistent, then call `clear_poison`. Calling
`into_inner` on the `PoisonError` merely to make the error go away is the defect.
Whether unsafe code may rely on poisoning is `reviewing-unsafe-rust`'s question; it
may not.

## Designing the error type

Pick the shape by who reads the error. A library's reader is code that branches; an
application's reader is a person.

**Default, overridable — three shapes:**

1. **Library, failures the caller can act on, no payload needed.** A `#[non_exhaustive]`
   enum with one variant per failure the caller would handle differently. It costs no
   allocation, and gives the caller a `match`. Derive `Copy` when its variants carry
   no data; Google recommends `Copy` errors wherever the fields allow it.
2. **Library, error must carry context or a source** (a path, an upstream error, a
   position). Use a struct over a private representation, with a `kind()` accessor
   returning a `#[non_exhaustive]`, `Copy`, fieldless kind enum. This is how
   `std::io::Error` is built. The representation can change in a minor release while
   the kinds stay matchable — in matklad's words, such an enum's "design space is a
   point". Box large payloads: an error's size is paid on the success path too,
   because `Result<T, E>` is at least as large as its larger side.
3. **Application.** One opaque error that accumulates context on the way out (the
   `anyhow` or `eyre` shape). It belongs at the top of an application, and never in a
   library's public API, where it erases the distinctions callers branch on. Choose
   one such type per application and use it throughout; two of them mean conversions
   that add nothing.

Microsoft's guidelines make every error a "canonical struct" with a backtrace, and
hide the kind behind `is_xxx()` methods. That struct is adopted here only for shape 2.
The kind stays public, because a `match` with a compiler-enforced wildcard is a better
caller experience than a growing set of predicates, and `#[non_exhaustive]` already
supplies the future-proofing the predicates were for. Override toward Microsoft's form
when the kinds would expose failure modes you consider internal and unhandleable;
collapse those into one variant instead of publishing each.

Scope error types to operations, not to the crate. A download and a VM start that
share no failure modes should not share a crate-wide enum that makes every caller
handle both. A JSON parser and a TOML parser that fail the same ways can share one.

Mechanics, for every shape:

- Implement `std::error::Error` and `Display`. The `Display` text is lowercase,
  concise, and has no trailing punctuation, so it composes into a chain.
- Make it `Send + Sync + 'static`, so it crosses threads, fits in an `Arc`, and can be
  wrapped by `io::Error::new`. Errors that cross an async or thread boundary must not
  borrow from the frame that produced them; designing that in early is cheaper than
  retrofitting it.
- Never use `()` as an error type. A unit struct with `Display` and `Error` costs
  nothing and composes with `?`.
- Keep the cause. Return it from `source()` so a message three layers up is still
  diagnosable.
- Convert with `impl From<Upstream> for Error` once, and let `?` apply it. Reserve
  `map_err` for adding context. But every public `From` impl makes the upstream type
  part of your API, so a shape-2 error converts into its *private* representation.

**Default, overridable — backtraces only in applications, or behind a feature.**
`Backtrace::capture()` is nearly free when `RUST_BACKTRACE` is unset and expensive when
it is set; Microsoft measured about 4 µs per capture. A library error constructed on a
hot path must not pay that whenever an operator turns backtraces on to debug
something else.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| `panic!`, `unwrap`, or `expect` on input a correct caller can send | Return `Result` |
| `Err` variant that means "this library has a bug" | Panic, and document it under `# Panics` |
| A panicking API with no fallible twin, where callers cannot prove the precondition | Add `get`/`checked_`/`try_` beside it |
| `if x.is_some() { x.unwrap() }` | `if let Some(v) = x` |
| `match` whose only arm returns the error unchanged | Replace with `?` |
| Nested `match` to bind one value or return | `let ... else` |
| `.unwrap()` far from the reason it cannot fail | `.expect("why this cannot fail")` |
| `assert!` message restating the condition | State why it should hold, or the values that broke it |
| `catch_unwind` in a library, or in a `panic = "abort"` build | Remove it |
| Panic caught, then the same state reused | Discard the touched state; restart the unit |
| `PoisonError::into_inner` with no restore or check | Propagate with `expect`, or restore a known-good value and `clear_poison` |
| Library returns `Box<dyn Error>` or `anyhow::Error` | Enum, or a struct with `kind()` |
| Public error enum without `#[non_exhaustive]` | Add it; a new variant is otherwise a breaking change |
| One crate-wide error enum spanning unrelated operations | One error type per family of operations |
| `.map_err(MyError::Io)?` repeated at call sites | `impl From<io::Error>`, then `?` |
| `Result<T, ()>` | A unit-struct error implementing `Error` |
| Error not `Send + Sync` | Own the data; no borrows, no `Rc` |
| `Backtrace::force_capture()` in a library error | `capture()`, in an application or behind a feature |

## Review checklist

- Could a correct program reach each panic? If so, it should be a `Result`.
- Does each `Err` describe something the caller can act on?
- Does every `expect` say why, and every non-obvious assertion carry context?
- Is any `catch_unwind` in library code, in an `abort` build, or followed by reuse of touched state?
- Is any poisoned lock entered without restoring or checking the data?
- Is each public error enum `#[non_exhaustive]`, `Send + Sync + 'static`, and does it implement `Error`?
- Does any public `From` impl leak an upstream type the crate did not mean to commit to?
- Is a backtrace captured on a hot path?

## Anti-patterns

- An expected failure that panics, or a bug reported as an `Err`.
- `unwrap` in library code, with the reachability argument left in someone's head.
- `catch_unwind` used as try/catch.
- `into_inner` on a poisoned lock to silence the error.
- An opaque or boxed error in a library, forcing callers to string-match.
- One crate-wide kitchen-sink error enum.
- A wrapper error that discards what it wrapped.

## Sources

Verified against these on 2026-11-05:

- std: `std::io::Error`, `ErrorKind`, `panic::catch_unwind`, `sync::Mutex` (Poisoning).
- Rust API Guidelines: C-GOOD-ERR, C-FAILURE.
- Google Rust Style Guide: "Reporting errors", "Prefer pattern matching over check-then-unwrap", "Prefer `expect` over `unwrap`", "Use context strings to clarify assertions", "Handling poison", "Don't call `panic::catch_unwind`", important traits (error types `Copy`).
- Microsoft Pragmatic Rust Guidelines: M-PANIC-IS-STOP, M-PANIC-ON-BUG, M-PANIC-CONTINUATION, M-PANIC-MESSAGE, M-ERRORS-CANONICAL-STRUCTS, M-FROM-ERROR, M-APP-ERROR.
- Andrew Gallant (BurntSushi), "Using unwrap() in Rust is Okay".
- Aleksey Kladov (matklad), "Study of std::io::Error".
