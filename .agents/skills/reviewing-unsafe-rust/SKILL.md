---
name: reviewing-unsafe-rust
description: Use when writing or reviewing an unsafe block, a safe wrapper over unsafe internals, a raw pointer, a union, or an FFI boundary — establishing what each unsafe operation requires and who is responsible for guaranteeing it.
uses:
  - name: writing-idiomatic-rust
    source: audacioustux/agents
  - name: hardening-rust-supply-chain
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

Ordinary ownership and API-shape questions belong to `writing-idiomatic-rust`.
Auditing dependencies for unsafe code belongs to `hardening-rust-supply-chain`.

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

For the same reason, do not alias, re-export, or wrap unsafe operations under names
that hide their nature. The word is a warning channel, and renaming it closes the
channel while leaving the hazard.

## FFI boundaries

Foreign code has none of Rust's guarantees, so the boundary is where they have to be
re-established.

Anything crossing in is untrusted. A pointer from foreign code needs the same five
checks as any other raw pointer, plus a length that is validated rather than assumed —
including an upper bound, since a plausible-looking length can still be absurd.

Ownership has to be settled explicitly and in one direction. Which side allocates,
which side frees, and with which allocator. Freeing foreign memory with Rust's
allocator, or the reverse, is undefined behaviour that usually appears to work.

A Rust panic must not escape into foreign frames. Since Rust 1.81 an unwind reaching
an `extern "C"` boundary aborts the process rather than causing undefined behaviour —
safer than it was, but still a crash your caller cannot handle. Any `extern` entry
point that could panic needs the unwind caught at the boundary and converted into
something the other side understands, typically an error code.

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
| Raw deref justified by a null check alone | State all five: non-null, aligned, initialised, aliasing, still-valid |
| Two `&mut` derived from one raw pointer | UB whether or not both are used; restructure so only one exists |
| `unsafe impl Send`/`Sync` with no comment | State the per-field argument beside it |
| Safe `fn` that documents a precondition it does not check | Validate at the boundary, or make the function `unsafe fn` |
| `unsafe fn` with no `# Safety` section | Add one; it is the only channel the caller sees |
| Wrapper or alias that hides `unsafe` in its name | Keep the word; renaming closes the warning channel |
| Length from foreign code used unvalidated | Bound it, upper limit included |
| `extern` entry point that can panic | Catch the unwind and return an error code; it aborts otherwise |
| Struct shared with C using the default representation | `#[repr(C)]`; the default reorders fields |
| Unsafe paths covered only by `cargo test` | Run them under Miri; Clippy sees none of this |

## Review checklist

- Which operation needs the `unsafe` — raw deref, unsafe call, mutable static, unsafe impl, union read, inline asm, or a 2024 unsafe attribute — and does the block wrap only that?
- For each dereference: non-null, aligned, initialised, aliasing-respecting, still valid?
- Does any safe function let a caller reach undefined behaviour with valid-looking input?
- Does every `unsafe fn` carry a `# Safety` section stating the caller's obligation?
- Is every manual `Send`/`Sync` justified against every field?
- At each FFI boundary: lengths bounded, ownership assigned, unwind caught, layout guaranteed?
- Has Miri run over tests that actually reach the unsafe paths under review?

## Anti-patterns

- A large `unsafe` block wrapping one unsafe line.
- Reaching for `unsafe` because the borrow checker rejected the design.
- A null check presented as a soundness argument.
- A safe function documenting a precondition it does not enforce.
- Trusting a clean Clippy run as evidence about unsafe code.
- Treating a "five capabilities" list as current: inline asm and, in edition 2024, unsafe attributes also qualify.
- Assuming an escaping panic is UB rather than an abort, and skipping the catch because "it works".
