---
name: designing-rust-apis
description: Use when designing or reviewing the public surface of a Rust crate or module — naming methods, constructors and conversions, choosing which traits to implement or derive, type aliases versus newtypes, `bool` parameters, builders, `#[must_use]` or `#[non_exhaustive]`, feature flags, re-exports, doc sections, or a change that might break semver.
uses:
  - name: writing-idiomatic-rust
    source: audacioustux/agents
  - name: handling-rust-errors
    source: audacioustux/agents
  - name: optimizing-rust-hot-paths
    source: audacioustux/agents
  - name: configuring-rust-lints
    source: audacioustux/agents
  - name: evolving-shared-contracts
    source: audacioustux/agents
---

# Designing Rust APIs

## Overview

A public item is a promise. Every `pub` type, field, variant, trait impl, and feature
is something a caller can depend on, and something you can no longer change in a
minor release.

Good Rust APIs make wrong calls fail to compile and make right calls read like what
they do. The type system can do most of this, provided the signature asks it to.

## When to use

Use for:

- a new public function, type, trait, module, or feature flag
- review of a crate's surface before a release
- a change to an existing public item

How strictly to apply it depends on the code. Google's guide makes these rules
binding for library code — anything another project consumes — and a strong
recommendation for application code, which should favour directness over
generality. Calibrate the same way: in a library a violation is a finding, in a
single binary it is a judgement call.

Error types belong to `handling-rust-errors`; ownership in signatures and dispatch to
`writing-idiomatic-rust`; allocation-aware API shapes to `optimizing-rust-hot-paths`.
Rolling out a breaking change across consumers follows `evolving-shared-contracts`,
if installed.

## Types carry meaning

**Newtypes, not aliases, for distinct meanings.** `type UserId = u64` is a second
name for `u64`, and the compiler will accept a byte count where a user id was meant.
`struct UserId(u64)` is a different type. Use a newtype when values are easy to mix
up, when the type deserves its own methods, or when you may change the
representation later. Keep aliases for shortening long generic types.

**A newtype that exists for an invariant enforces it.** A `Month(pub u8)` enforces
nothing, and every caller must re-check it. Keep the field private and give a
fallible constructor (`try_new`, `TryFrom`, `FromStr`). A panicking `new` may sit
beside it, ideally as a `const fn`, so `const { Month::new(13) }` fails at compile
time. Never offer an infallible `From` from the weaker type.

**Enums, not `bool`, for parameters whose meaning is not obvious at the call site.**
`Cache::new(1024, true, false)` cannot be read without the definition;
`Cache::new(1024, Metrics::On, Eviction::Lenient)` can. Where a function is always
called with literal `true` or `false`, consider two functions instead. The same goes
for `Option` arguments used as switches.

**Named fields over tuples**, except single-field newtypes and very local code. `.0`
and `.1` stop meaning anything a few lines from where the tuple was built.

**The strongest standard type available.** Paths are `Path`/`PathBuf`, not `str`;
durations are `Duration`, not a `u64` of unknown unit. Plain numbers stay plain at
public boundaries, though — std does not hand callers `NonZero` or `Saturating` for
an ordinary size.

## Naming

Follow std, because callers already know it.

| Shape | Name | Meaning |
| --- | --- | --- |
| Free view, borrowed to borrowed | `as_foo` | No cost: `str::as_bytes` |
| Costly conversion, or borrowed to owned | `to_foo` | Does work: `str::to_lowercase`, `Path::to_str` |
| Consumes `self` | `into_foo` | `String::into_bytes`; `into_inner` unwraps a wrapper |
| Getter | `foo()`, `foo_mut()` | No `get_` prefix, unless there is one obvious thing to get (`Cell::get`) |
| Setter, replace, take | `set_foo`, `replace_foo`, `take_foo` | `replace` and `take` return the old value |
| Constructor | `new`, `try_new`, `with_foo`, `from_foo` | Associated functions; `try_new` returns `Result`; `from_foo` for conversions `From` cannot express (unsafe, extra arguments, or input whose meaning the type alone does not fix: `String::from_utf8`, `Box::from_raw`) |
| Iterators | `iter`, `iter_mut`, `into_iter` | Returning `Iter`, `IterMut`, `IntoIter` |
| Extension trait | `FooExt` | Adds methods to a type you do not own |

Implement API sets together. A type with `len()` also has `is_empty()`. A collection
implements `IntoIterator` for itself, `&Self`, and `&mut Self`, plus `FromIterator`
and `Extend`, and its iterators report a truthful `size_hint`, which `collect` uses to
allocate once.

Do not abbreviate past what an outsider would recognise, and do not drop letters out of
words. Drop words that add nothing: a type that holds bookings is `Bookings`, not
`BookingManager` or `BookingService`, and a `FooFactory` is a builder by another name.
Associated functions are for constructing the type. A computation with no receiver is
a free function in the module, not a static method on a type that merely hosts it.

## Traits

**Implement the standard trait rather than a lookalike method.** `impl FromStr` rather
than an inherent `from_str`; `impl Default` rather than only a zero-argument `new` (or
both, with `new` calling `default`). Use `#[derive]` wherever it produces the right
behaviour.

**Derive the common traits eagerly in libraries.** A downstream crate cannot add
`Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`, `PartialOrd`, `Ord`, or `Default` to your
type, because of the orphan rule. Derive each one that is semantically true. Add
`Copy` only when the type will stay `Copy`, since removing it later breaks callers.

**Every public type is `Debug`.** A type holding secrets implements `Debug` by hand,
redacts them, and carries a test asserting the secret never appears in the output.
**Types meant to be read are also `Display`** — errors, and anything string-like.

**Do not put derived traits in a struct's bounds.** `struct Bag<T: Clone>` forces the
bound on every user. `#[derive(Clone)] struct Bag<T>` already applies it only where
needed.

**Essential functionality is inherent.** If a type's main operation lives only in a
trait impl, callers must discover and import the trait to call it. Write the inherent
method, and have the trait impl forward to it.

**Define a trait for generic code, not to organise methods.** A trait with one
implementation is indirection, as `writing-idiomatic-rust` § Dispatch explains. Seal
a trait (a private supertrait) when only your crate should implement it. That keeps
the freedom to add methods without a breaking release.

**Types used with a work-stealing runtime are `Send`.** On a multi-threaded executor
such as tokio's default, a non-`Send` value held across an await makes the whole future
non-`Send`, so it cannot be spawned. Assert it for key types and futures at compile
time: `const _: () = { const fn check<T: Send>() {} check::<Client>(); };`. A
thread-per-core API may be `!Send` on purpose — `Rc` and per-core state avoid atomics
there, as `writing-idiomatic-rust` § Shared ownership allows — and says so in its docs.
Microsoft's guidelines lean the other way for ecosystem reach: an occasional
uncontended atomic costs little, and `!Send` locks callers out of tokio. Pick `Send`
when the type will meet a work-stealing runtime. `Sync` is not a default requirement;
add it when shared references genuinely cross threads.

## Future-proofing

**`#[non_exhaustive]` on public enums** that may gain variants, and on public structs
with public fields that may gain fields. Without it, adding either is a breaking
change. Leave it off a closed ontology, such as cardinal directions or a two-state
enum, and off an options struct meant for `..Default::default()`. Within your own
crate, `#[non_exhaustive]` has no effect.

When matching *someone else's* non-exhaustive enum, list every variant you know and
put only the unknown future in `_`. The wildcard still swallows a new upstream variant
silently, so treat a dependency upgrade as the time to re-read those matches against
its changelog.

**Fields are private by default.** A public field can never gain an invariant or
change its representation. Make a field public only when any value is valid and the
type is plain data.

**`#[must_use]` where ignoring the result is a bug:** pure functions, builders, and
values that do nothing until used. `Result` is already `#[must_use]`, so a function
returning one needs no annotation of its own. Put it on a type when every function
returning that type would need it.

**`const fn` is a commitment.** Removing `const` breaks callers. Mark a public function
`const` only if it will stay `const`.

**Features are additive.** Cargo unifies features across the whole dependency graph, so
any combination must compile and behave. Enabling a feature must not remove or change
a public item. Use a `std` feature rather than a `no_std` one. A feature that adds enum
variants needs that enum to be `#[non_exhaustive]`. Removing a feature, or moving
existing code behind one, is a breaking change.

**Edition and MSRV.** New crates use the latest stable edition; it costs downstream
nothing. Declare `rust-version` when the crate is created, and keep it a few releases
behind stable. Raising it is a minor-version change, not a major one.

## Construction

With up to two optional parameters, inherent constructors are enough: `new`, `with_a`,
`with_b`, `with_a_b`. From three on, the combinations double with each parameter, so
provide a builder.

- `Foo::builder(required)` returns `FooBuilder`. Required inputs go in at creation,
  never as setters someone can forget.
- Setters are named after the field (`timeout`, not `set_timeout`), take and return
  `self`, and never fail.
- `build()` does all validation, including checks across fields, and returns
  `Result` if anything can be invalid.
- A struct whose fields are all public options implements `Default`, so callers write
  `Opts { retries: 3, ..Default::default() }`.

## Keep I/O at the edges

Logic that parses, decides, or transforms should not own a socket, a file, or the
clock.

- **Sans-I/O.** Put the logic in a core that takes and returns bytes and state, and
  never reads or writes itself: `fn feed(&mut self, input: &[u8]) -> Events`. Thin
  adapters at the edges drive it from a socket, a file, or an async stream. Where a
  one-shot blocking reader is all you need, accept `impl Read`/`impl Write` (by value;
  `&mut R` also implements them) rather than a `File` or a path. That abstracts the
  source, but it still blocks on it, so it is not sans-I/O.
- **Mockable effects.** A type that touches the network, filesystem, clock, or entropy
  takes that capability as input, or holds a private enum of `Real` and (behind a
  feature) `Mock`. `Library::new_mocked() -> (Library, MockCtrl)` returns the
  controller, so two instances never share one by accident.
- **Test utilities behind one feature**, conventionally `test-util`. That covers
  mocks, fake data, and any bypass of a safety check.
- **No statics for state whose consistency matters.** Two semver-incompatible versions
  of your crate in one graph means two copies of every static, silently. A static used
  only as a cache is fine.
- **No smart pointers in signatures.** `Arc<Mutex<T>>`, `Rc<RefCell<T>>`, and
  `Box<dyn Trait>` in a public signature leak an implementation decision. Take `&T`,
  `&mut T`, or `T`, and hide the wrapper inside a named type.

## Module surface

**Each public item has one path.** Re-exporting `db::Connection` as `crate::Connection`
while `db` is public gives readers two names for one thing. Make the inner module
private, or do not re-export.

**No glob imports**, except `use super::*` in an inline module (typically tests) and
a local `use Enum::*` inside a function that is mostly a `match` on that enum. A glob
hides where a name came from, and two globs that grow the same name stop compiling.

**Default, overridable — no glob re-exports except a deliberate prelude.** Microsoft
forbids preludes outright, because two crates' preludes collide with an ambiguity
error. Google discourages defining them but allows `pub use submodule::*` in non-leaf
modules. The resolution: re-export items by name. A glob re-export is acceptable only
for a module that exists to be a prelude — small, stable, documented as such — or to
forward exactly one of several `cfg`-selected platform modules.

## Documentation

- Every public item has a doc comment. The first sentence is a one-line summary,
  because rustdoc shows it in listings.
- Use the canonical sections whenever they apply: `# Errors` for every function
  returning `Result`, `# Panics` for every panic a caller can trigger, `# Safety` for
  every `unsafe fn` and `unsafe trait`. Clippy's `missing_errors_doc`,
  `missing_panics_doc`, and `missing_safety_doc` check them; `configuring-rust-lints`
  turns them on.
- Examples use `?`, never `unwrap`, because readers copy them.
- Describe parameters in prose, not a parameter table, and do not restate the
  signature.
- A magic number gets a named constant whose doc says why that value, what breaks if
  it changes, and which outside system depends on it.
- Document the end state, not the journey. Design narratives, "why we chose X over Y"
  essays, and checklists of guidelines applied go stale in user-facing docs; they
  belong in the commit or the design record.

## Diagnostics

Libraries emit diagnostics through a logging facade such as `tracing` or `log`, never
`println!` or `dbg!`. Console output belongs to a binary's user interface. Log
structured fields (`path = %p`) rather than pre-formatted strings, give events stable
names, and redact secrets and personal data before they reach a field. The cost of
logging on a hot path belongs to `optimizing-rust-hot-paths`.

## Macros

Macros are a last resort, because readers and tools can assume less about a call site.
In order of preference: a function, type, or trait; then `macro_rules!`; then a
procedural macro, in its own crate. A macro must not disguise what it produces: no
turning a struct into an enum, no silent extra parameters, no hidden `async`. Inside a
macro body, refer to items by absolute path (`$crate::`, `::core::`), so expansion does
not depend on the caller's imports. Never let an expansion hide an `unsafe` block from
the caller.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| `type Id = u64;` used where ids and counts could mix | `struct Id(u64);` |
| Newtype guarding an invariant, with a `pub` field or infallible `From` | Private field; fallible constructor or `TryFrom`. A newtype that accepts every value, like `UserId(u64)`, may offer `From` |
| `f(x, true, false)` | Enums for the flags, or separate functions |
| `get_name()` getter | `name()` |
| `as_*` method that allocates | Rename to `to_*` |
| Inherent `from_str`, or `new()` with no `Default` | `impl FromStr`; `impl Default` |
| Public type with no `Debug` | `#[derive(Debug)]`, or a redacting impl plus a test |
| `struct Foo<T: Clone + Debug>` with matching derives | Drop the bounds from the struct |
| Public enum or all-`pub` struct without `#[non_exhaustive]` | Add it, unless it is a closed ontology or an options struct |
| `_ =>` arm swallowing known variants of a foreign non-exhaustive enum | List the known variants; `_` for the future only |
| Pure `pub fn` or builder method without `#[must_use]` | Add it |
| `no_std` feature, or a feature that removes items | `std` feature; additive only |
| Fallible builder setters (`.name(x)?`) | Infallible setters; `build()` returns `Result` |
| `fn parse(path: &Path)` that only reads bytes | A byte-slice core, with `impl Read` or a path only at the edge |
| `SystemTime::now()` or `rand::random()` deep inside logic | Take a clock or RNG as input |
| `pub fn f(x: Arc<Mutex<T>>)` | `&T`/`&mut T`; hide the sharing |
| `pub use inner::*` | Re-export by name |
| Doc example ending in `.unwrap()` | `?`, with a hidden `fn main() -> Result<..>` |

## Review checklist

- Can a caller mix up two values of the same primitive type? Should either be a newtype?
- Does every newtype that exists for an invariant enforce it at construction?
- Can each call site be read without the definition — no bare `bool` or `None` switches?
- Do names follow `as_`/`to_`/`into_`, getter, and constructor conventions?
- Is every public type `Debug`, and are the semantically true common traits derived?
- Which public enums and structs can grow, and are they `#[non_exhaustive]`?
- Is every feature additive, and every feature combination buildable?
- Is I/O, time, and randomness injected rather than reached for?
- Does every public item have exactly one path and complete doc sections?

## Anti-patterns

- Type aliases standing in for distinct domain types, and newtypes that guard nothing.
- Exhaustive public enums in a library expected to grow.
- Business logic that opens its own files and reads its own clock.
- A prelude added to paper over a module layout that is hard to navigate.

## Sources

Verified against these on 2026-11-05:

- Rust API Guidelines: C-CONV, C-GETTER, C-ITER, C-COLLECT, C-COMMON-TRAITS, C-DEBUG, C-STRUCT-BOUNDS, C-SEALED, C-STRUCT-PRIVATE, C-NEWTYPE, C-CUSTOM-TYPE, C-BUILDER, C-CTOR, C-FAILURE, C-QUESTION-MARK, C-SEND-SYNC, C-RW-VALUE.
- Google Rust Style Guide: "Apply API design rules in context", "Newtypes versus type aliases", "Prefer `enum` over `bool` in APIs", "Prefer named fields", naming sections, "Getters and setters", "Implement expected API sets together", traits sections, `#[non_exhaustive]` and `#[must_use]` sections, "Use `const` functions responsibly", "Don't use glob imports", "Macros", documentation.
- Microsoft Pragmatic Rust Guidelines: M-WEASEL-WORDS, M-REGULAR-FN, M-NO-META-DESIGN-DOCUMENTATION, M-STRONG-TYPES, M-STRONG-TYPES-GUARD, M-PUBLIC-DEBUG, M-PUBLIC-DISPLAY, M-ESSENTIAL-FN-INHERENT, M-COLLECTION-TRAITS, M-TYPES-SEND, M-INIT-BUILDER, M-BUILD-RESULT, M-IMPL-IO, M-MOCKABLE-SYSCALLS, M-TEST-UTIL, M-AVOID-STATICS, M-AVOID-WRAPPERS, M-SINGLE-ITEM-PATH, M-NO-GLOB-REEXPORTS, M-NO-PRELUDE, M-FEATURES-ADDITIVE, M-LATEST-EDITION, M-MSRV, M-CANONICAL-DOCS, M-DOCUMENTED-MAGIC, M-LOG-STRUCTURED, M-LOG-NOT-PRINT, M-MACRO-LAST-RESORT, M-EXAMPLE-OVER-PROC, M-MACROS-DONT-LIE.
- Cargo reference: Features (feature unification, mutually exclusive features, SemVer compatibility).
- The Rust Reference: diagnostic attributes (`must_use`).
- rust-analyzer style guide: "Prefer Separate Functions Over Parameters".
- Safety-Critical Rust Coding Guidelines (draft): fully qualified paths in macros; unsafe hidden in macro expansions.
