---
name: reviewing-unsafe-rust
description: Use when writing or reviewing an unsafe block, a safe wrapper over unsafe internals, a raw pointer, a union, or an FFI boundary — establishing what each unsafe operation requires and who is responsible for guaranteeing it.
uses:
  - name: writing-idiomatic-rust
    source: audacioustux/agents
  - name: hardening-rust-supply-chain
    source: audacioustux/agents
  - name: handling-rust-errors
    source: audacioustux/agents
  - name: configuring-rust-lints
    source: audacioustux/agents
---

# Reviewing Unsafe Rust

## Overview

`unsafe` does not switch off the borrow checker. It unlocks a specific, enumerable set
of operations the compiler cannot verify, and leaves every other rule in force.

Reviewing unsafe code is therefore not a search for danger in general. It is checking,
per operation, what that operation requires and whether something guarantees it.

Verified against stable Rust as of 1.90, editions 2021 and 2024.

## When to use

Use for:

- any `unsafe` block or `unsafe fn`
- a safe API whose implementation is unsafe
- raw pointers, unions, `static mut`, or manual `Send`/`Sync`
- any FFI boundary in either direction

Ordinary ownership and dispatch questions belong to `writing-idiomatic-rust`.
Auditing dependencies for unsafe code belongs to `hardening-rust-supply-chain`.
Where `unsafe` is allowed in a crate, and which lints check it, is
`configuring-rust-lints`.

## What unsafe actually grants

Five operations, plus two cases that arrived later. The classic five: dereferencing a
raw pointer, calling an unsafe function, accessing or modifying a mutable static,
implementing an unsafe trait, and reading a union field. Each is genuinely gated —
omitting `unsafe` is `E0133` for the operations, `E0200` for the trait impl. Inline
assembly is a sixth, gated the same way. In edition 2024, attributes such as
`#[no_mangle]` also require an `unsafe(...)` wrapper, so a list of "five" is now the
historical framing rather than the current count.

Everything else follows safe rules inside the block. Borrowing, lifetimes, and
allocation behave as they always do — a use-after-borrow inside an `unsafe` block is
still `E0502`. This matters because a block wrapping fifty lines to cover one
dereference tells a reviewer nothing about which line is load-bearing. Keep the block
around the operation that needs it.

That principle has a consequence worth stating on its own: `unsafe` is not an escape
hatch. Reaching for it because the borrow checker rejected a design converts a
compile-time error into a runtime one and moves the proof obligation from the compiler
to a person who may not know they inherited it.

Nor is it a performance tool by default. Bounds checks are frequently eliminated
where the optimiser can prove the index is in range, so removing them by hand often
buys nothing. Measure before trading a guarantee for a number nobody has seen.

That leaves three valid reasons: a new abstraction std and the ecosystem do not
already provide (a container, an allocator); a measured performance gain; and FFI
or platform calls. `unsafe` used ad hoc to shorten safe code, to `transmute` an enum,
to stretch a lifetime, or to `unsafe impl Send` past a bound has none of them.

## Write the argument down

Every `unsafe` block carries a `// SAFETY:` comment directly above it, stating why
each precondition holds *here* — not what the operation does. Clippy's
`undocumented_unsafe_blocks` enforces the comment's presence, and
`multiple_unsafe_ops_per_block` keeps each block to one operation, so each comment
answers for one obligation. Neither lint can judge the argument; a comment saying
"this is safe" passes both. Lint configuration is `configuring-rust-lints`.

Where the reason is performance, the justification also says why the safe version
was not enough, and which benchmark showed it.

Keep unsafe code small and isolated, ideally in a module or crate of its own with few
dependencies, so business logic never sits inside a block. Deny `unsafe_code`
everywhere else, and the build shows where the obligations live.

## Preconditions, per operation

A raw pointer dereference requires all of: the pointer is non-null, it is aligned for
its type, the memory is initialised for that type, the aliasing rules hold, and the
memory stays valid for the lifetime of any reference produced from it. All five, every
time. A null check alone is not a soundness argument.

Aliasing is the one most often skipped, because nothing crashes when it is violated.
Producing two `&mut` to the same location is undefined behaviour whether or not both
are used, and the optimiser is entitled to assume it never happened.

A manual `Send` or `Sync` implementation is a claim about every field, transitively.
The compiler derives these automatically wherever it can, so writing one by hand means
overriding a decision it declined to make, and the reason belongs in a comment beside
it.

Reading a union field asserts that the field last written is the field being read.
Nothing tracks that; the code has to.

## Safe abstractions

The purpose of a safe wrapper is to make the unsafe operation unreachable in a way
that violates its preconditions. If a caller can trigger undefined behaviour without
writing `unsafe` themselves, the wrapper is unsound, however small the gap.

Which means a safe function containing unsafe code owes an argument that no input can
break it. Validate at the boundary — lengths, null, alignment, ranges — rather than
documenting the requirement and hoping. A documented precondition on a safe function
is not enforcement; the safety marker the caller would have seen is exactly what the
wrapper removed.

Where the caller genuinely must uphold something, the function should be `unsafe fn`
and say so in a `# Safety` section. That is not a formality: it is the only mechanism
that puts the obligation where the caller can see it, and it restores the marker the
wrapper would otherwise have deleted.

The converse holds too: mark a function `unsafe` only when misuse risks undefined
behaviour. A function that is merely dangerous — deleting data, ending the process —
stays safe and says so in its docs. Spending the marker on hazards it does not mean
teaches readers to ignore it.

For the same reason, do not alias, re-export, or wrap unsafe operations under names
that hide their nature, and do not let a macro expand to an `unsafe` block the caller
never wrote. The word is a warning channel, and renaming or generating it away closes
the channel while leaving the hazard.

Soundness has no "theoretical" exemption. A safe function is unsound if *any* call
from safe code can cause undefined behaviour, however contrived the call. The
boundary of the argument is the module. A safe method may rely on an invariant that
every other function in the same module upholds, because private fields keep code
outside the module from breaking it. Code in another module gets no such trust.

Assume caller-supplied code misbehaves. A `Clone`, `Deref`, `Drop`, `Ord`, or `Hash`
implementation on a user's type can return nonsense, panic, or be inconsistent between
calls, and safe code is allowed to do all of that. So an unsafe abstraction must stay
sound when it does. A closure that panics halfway through an operation must leave the
structure valid: restore the invariant in a guard's `Drop`, or mark the structure
poisoned. Lock poisoning itself is no help here; std documents that unsafe code cannot
rely on it for soundness.

## FFI boundaries

Foreign code has none of Rust's guarantees, so the boundary is where they have to be
re-established.

Anything crossing in is untrusted. A pointer from foreign code needs the same five
checks as any other raw pointer, plus a length that is validated rather than assumed —
including an upper bound, since a plausible-looking length can still be absurd.

Ownership has to be settled explicitly and in one direction. Which side allocates,
which side frees, and with which allocator. Freeing foreign memory with Rust's
allocator, or the reverse, is undefined behaviour that usually appears to work.

A Rust panic must not unwind into foreign frames that do not expect it. Since Rust
1.81 a panic reaching an `extern "C"` boundary aborts the process, rather than causing
undefined behaviour. What to do beyond that depends on the panic strategy and on
whether the caller can live with an abort:

| Situation | Rule |
| --- | --- |
| Abort is acceptable | Plain `extern "C"` is enough; std's own docs say no `catch_unwind` is needed |
| Caller must get an error, and the build uses `panic = "unwind"` | Wrap the body in `catch_unwind` and return an error code |
| `panic = "abort"` | `catch_unwind` catches nothing. Prevent panics on that path instead: validate inputs, return `Result` internally |
| Unwinding is meant to cross into frames that handle it | `extern "C-unwind"`, on both sides |

A library cannot know which strategy its final binary uses, so an FFI entry point that
must not abort should not panic at all. The `catch_unwind` is the backstop, not the
design. Foreign exceptions are worse than Rust panics. One entering Rust through a
non-unwind ABI is undefined behaviour, and catching one with `catch_unwind` has
unspecified results. Panic handling elsewhere is `handling-rust-errors`.

Types crossing the boundary need a guaranteed layout. Rust's default representation
promises nothing about field order or padding, and it genuinely reorders: a struct of
`u8, u32, u8` lays out as `a` at offset 4, `b` at 0, `c` at 5 under `repr(Rust)`,
against 0/4/8 under `repr(C)`. Anything shared with C needs the explicit C
representation rather than an assumption that fields land where they look like they do.

## Verification

Miri interprets Rust and detects undefined behaviour that testing alone will not
surface: out-of-bounds access, use-after-free, misaligned access, and aliasing
violations. The gap is not theoretical. A crate containing an out-of-bounds read and
a double-`&mut` passes `cargo test` green on both, and `cargo miri test` fails with
"error: Undefined Behavior" on each. Miri runs only the code paths your tests reach,
so its coverage is your tests' coverage — and it stops at the first violation, so a
clean second run after a fix is not proof that there was only one.

Clippy catches only a small share of unsafe defects, and that is by design rather
than a gap to be closed — most soundness obligations are not lint-shaped. On the same
out-of-bounds read, Clippy exits zero with no warnings at all. Treat a clean lint run
as no evidence whatsoever about unsafe correctness.

Sanitisers are the third layer, catching at run time what static analysis cannot
reach. None of these substitute for the per-operation argument; they check it.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| `unsafe` block spanning many lines for one operation | Shrink it to the operation; the rest follows safe rules anyway |
| `unsafe` added after a borrow-checker rejection | Revisit the design; the obligation moved from compiler to reader |
| `unsafe` used for speed with no measurement | Measure first; bounds checks are often already eliminated |
| `unsafe` block with no `// SAFETY:` comment, or one that restates the operation | State why each precondition holds here |
| One `unsafe` block covering several unsafe operations | One operation per block, one argument per comment |
| `transmute` between enums, or `unsafe impl Send` to get past a bound | Use the safe conversion; fix the type, or build a sound abstraction |
| Macro expanding to an `unsafe` block the caller never wrote | Make the caller write `unsafe`, or expose an `unsafe fn` |
| Safe method trusting a field another module can set | Make the field private; the module is the soundness boundary |
| Unsafe abstraction calling user `Clone`/`Ord`/`Drop` or a closure mid-update | Stay sound if they lie or panic: guard, or poison |
| Raw deref justified by a null check alone | State all five: non-null, aligned, initialised, aliasing, still-valid |
| Two `&mut` derived from one raw pointer | UB whether or not both are used; restructure so only one exists |
| `unsafe impl Send`/`Sync` with no comment | State the per-field argument beside it |
| Safe `fn` that documents a precondition it does not check | Validate at the boundary, or make the function `unsafe fn` |
| `unsafe fn` with no `# Safety` section | Add one; it is the only channel the caller sees |
| Wrapper or alias that hides `unsafe` in its name | Keep the word; renaming closes the warning channel |
| Length from foreign code used unvalidated | Bound it, upper limit included |
| `extern "C"` entry point that can panic, where abort is unacceptable | Remove the panic path; under `unwind`, also `catch_unwind` into an error code |
| `catch_unwind` at an FFI boundary in a `panic = "abort"` build | It catches nothing; make the path panic-free |
| Struct shared with C using the default representation | `#[repr(C)]`; the default reorders fields |
| Unsafe paths covered only by `cargo test` | Run them under Miri; Clippy sees none of this |

## Review checklist

- Which operation needs the `unsafe` — raw deref, unsafe call, mutable static, unsafe impl, union read, inline asm, or a 2024 unsafe attribute — and does the block wrap only that?
- Is the reason one of the three valid ones, and does a `// SAFETY:` comment argue each precondition?
- Does the abstraction stay sound if user trait impls or closures misbehave or panic?
- For each dereference: non-null, aligned, initialised, aliasing-respecting, still valid?
- Does any safe function let a caller reach undefined behaviour with valid-looking input?
- Does every `unsafe fn` carry a `# Safety` section stating the caller's obligation?
- Is every manual `Send`/`Sync` justified against every field?
- At each FFI boundary: lengths bounded, ownership assigned, panics handled for the actual panic strategy, layout guaranteed?
- Has Miri run over tests that actually reach the unsafe paths under review?

## Anti-patterns

- A large `unsafe` block wrapping one unsafe line.
- Reaching for `unsafe` because the borrow checker rejected the design.
- A null check presented as a soundness argument.
- A safe function documenting a precondition it does not enforce.
- Trusting a clean Clippy run as evidence about unsafe code.
- Treating a "five capabilities" list as current: inline asm and, in edition 2024, unsafe attributes also qualify.
- Assuming an escaping panic is UB rather than an abort — or that `catch_unwind` protects a `panic = "abort"` build.
- `// SAFETY: this is safe` — a comment that satisfies the lint and argues nothing.

## Sources

The 2026-11-05 additions were verified against:

- The Rustonomicon: FFI (unwinding, `C-unwind`).
- std: `panic::catch_unwind`, `sync::Mutex` (Poisoning).
- Google Rust Style Guide: "Unsafe code".
- Microsoft Pragmatic Rust Guidelines: M-UNSAFE, M-UNSOUND, M-UNSAFE-IMPLIES-UB.
- Clippy: `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`.
- Safety-Critical Rust Coding Guidelines (draft): do not hide `unsafe` within macro expansions.
