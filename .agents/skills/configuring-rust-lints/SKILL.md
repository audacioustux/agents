---
name: configuring-rust-lints
description: Use when setting up or reviewing a Rust crate's or workspace's lint configuration — the `[lints]` table in `Cargo.toml`, which Clippy groups or restriction lints to enable, `clippy.toml`, silencing a lint with `#[allow]` or `#[expect]`, confining `unsafe` to declared modules, or deciding which lint, format, and feature checks gate CI.
uses:
  - name: writing-idiomatic-rust
    source: audacioustux/agents
  - name: handling-rust-errors
    source: audacioustux/agents
  - name: designing-rust-apis
    source: audacioustux/agents
  - name: reviewing-unsafe-rust
    source: audacioustux/agents
  - name: hardening-rust-supply-chain
    source: audacioustux/agents
---

# Configuring Rust Lints

## Overview

Any rule a lint can check should be checked by the lint, not by a reviewer. A lint
runs on every change, never tires, and moves the argument from review time to the
configuration file, where it happens once.

What a lint cannot do is judge. The configuration decides which judgements a team
has already made. Every override in the source says where, and why, the team made an
exception.

## When to use

Use for:

- a new crate or workspace
- adding, removing, or raising a lint
- an `#[allow]` or `#[expect]` in review
- the lint and format steps of CI

The rules the lints enforce live with their owners: `writing-idiomatic-rust`,
`handling-rust-errors`, `designing-rust-apis`, and `reviewing-unsafe-rust`.
Dependency checks are `hardening-rust-supply-chain`.

## Where configuration lives

Put lint levels in `Cargo.toml` (`[lints]`, respected since Cargo 1.74), not in
`RUSTFLAGS`, `.cargo/config.toml`, or crate-root attributes. The manifest is versioned
with the code, read by every tool that runs Cargo, and applies only to your packages,
never to dependencies. In a workspace, declare the levels once under
`[workspace.lints]`, and give each member `[lints] workspace = true`.

Clippy's settings that are not levels — thresholds, disallowed types and methods,
test exemptions — go in `clippy.toml` at the workspace root. Clippy rejects unknown
keys there, so a typo fails loudly.

**Give every group `priority = -1`.** A group and a single lint at the same priority
is ambiguous, and Clippy's deny-by-default `lint_groups_priority` stops the build.
Lower priority loses, so the individual lines then override the group.

## Which groups

| Group | Default level | Policy |
| --- | --- | --- |
| `correctness` | deny | Leave on. Code it flags is wrong. |
| `suspicious`, `complexity`, `perf`, `style` | warn | Leave on. Follow `perf` even off the hot path. |
| `pedantic` | allow | Enable as a group at `priority = -1`, then opt out of named lints. |
| `restriction` | allow | Never as a group; Clippy itself warns if you try. Cherry-pick. |
| `nursery` | allow | Never as a group; its lints are still under development. Cherry-pick at most. |
| `cargo` | allow | Only for crates published to a registry. |

**Default, overridable — `pedantic` on as a group for libraries and long-lived
crates.** Microsoft enables it whole. Clippy's docs suggest cherry-picking instead,
and warn that it needs generous `#[allow]`s. Embark's standard set cherry-picks
68 Clippy lints. The group is preferred here because it carries the checks other
skills rely on: `missing_errors_doc`, `missing_panics_doc`, `must_use_candidate`,
`cast_possible_truncation`, `cast_sign_loss`, `needless_for_each`, and
`fn_params_excessive_bools`. Opting out of the few that do not fit, by name with a
comment, keeps the policy visible. The cost is that a toolchain bump can add
warnings, so pin the toolchain (`rust-toolchain.toml`) and take new lints on a
deliberate upgrade. Override with a cherry-picked list for a short-lived binary, or a
crate whose code is mostly generated.

## A baseline

Verified to parse and fire as described on rustc 1.100 nightly with Clippy 0.1.99. Treat
it as a starting point to prune, not a policy to adopt unread.

```toml
[workspace.lints.rust]
unsafe_code = "deny"                 # opt in per module, see below
missing_docs = "warn"                # libraries; drop for application-only crates
missing_debug_implementations = "warn"
unused_lifetimes = "warn"
redundant_lifetimes = "warn"
trivial_numeric_casts = "warn"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }

# Restriction lints, chosen one at a time.
undocumented_unsafe_blocks = "warn"     # every `unsafe {}` carries `// SAFETY:`
multiple_unsafe_ops_per_block = "warn"  # one obligation per block
allow_attributes_without_reason = "warn"
clone_on_ref_ptr = "warn"               # `Arc::clone(&x)`, visibly a count bump
dbg_macro = "warn"
todo = "warn"
map_err_ignore = "warn"                 # `map_err(|_| ..)` discards the cause
mem_forget = "warn"
print_stdout = "warn"                   # libraries: log through a facade
print_stderr = "warn"
exit = "warn"                           # libraries never end the process
```

Edition 2024 already warns on `unsafe_op_in_unsafe_fn`, so it is not repeated here; on
2021, add it.

## Restriction lints to leave off

`unwrap_used`, `expect_used`, `panic`, and `indexing_slicing` turn every panic into a
lint failure. That contradicts the default in `handling-rust-errors` — panic on a
detected bug — and pushes code toward `Err` variants nobody can act on, or toward
silently ignored results. Enable them only in a crate or module whose contract is
never to panic, such as a parser for untrusted input or firmware. Even then, exempt
tests in `clippy.toml` (`allow-unwrap-in-tests`, `allow-expect-in-tests`,
`allow-panic-in-tests`, `allow-indexing-slicing-in-tests`) rather than scattering
`#[allow]` across test modules.

`arithmetic_side_effects`, `as_conversions`, and `float_arithmetic` belong only in code
where every operation needs a stated overflow or precision policy: money, safety
cases, consensus. Elsewhere the pedantic cast lints cover the risky conversions.

## Confining `unsafe`

Set `unsafe_code = "deny"` for the whole workspace. In the one module that genuinely
needs `unsafe`, write
`#![expect(unsafe_code, reason = "FFI to libfoo; see SAFETY comments")]` (or the outer
form on the `mod` item). Then `unsafe` anywhere else fails the build, and the
exception names its scope and its reason. Use `forbid` instead of `deny` in a crate
that must never contain `unsafe`; `forbid` cannot be lowered from inside the crate.
What each `unsafe` block must argue is `reviewing-unsafe-rust`.

## `#[expect]` over `#[allow]`

**Default, overridable — `#[expect(lint, reason = "...")]`, on the smallest item that
needs it.** An `#[allow]` outlives the code it excused. An `#[expect]` warns
(`unfulfilled_lint_expectations`) once the lint stops firing, so stale exceptions
surface on their own. Microsoft requires this, and `allow_attributes_without_reason`
enforces the `reason`. Add `allow_attributes` only if you want `#[allow]` banned
outright. `#[expect]` is stable since Rust 1.81; a crate whose `rust-version` is older
must use `#[allow]` with a reason.

"I can't run the linter, so I'll use `allow` in case the lint doesn't fire" gets this
backwards. An unfulfilled expectation is a warning that names the dead exception, and
it is exactly what you want to learn on the next lint run. Run the linter before
calling the change done.

`#[allow]` with a reason remains right in three places:

- generated code and macro output, where the lint may or may not fire depending on
  the input;
- a lint that fires only under some `cfg`, feature, or target, where `#[expect]` would
  be unfulfilled on the others. Prefer `#[cfg_attr(feature = "x", expect(...))]` when
  the condition is exact;
- a crate-wide opt-out, which belongs in `[lints]` rather than an attribute.

Never silence a `correctness` lint without a comment showing why the code is right.

## CI

Keep levels at `warn` in the manifest, so local iteration is not blocked, and make CI
refuse warnings:

- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo fmt --all --check`
- `cargo hack check --feature-powerset` (or `--each-feature` for many features), so
  every feature combination builds — `designing-rust-apis` requires features to be
  additive
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`, so broken intra-doc links fail
- Miri over unsafe code, per `reviewing-unsafe-rust`; dependency policy, per
  `hardening-rust-supply-chain`

## Detection patterns

| Symptom | Fix |
| --- | --- |
| Lint levels in `RUSTFLAGS` or `.cargo/config.toml` | Move them to `[lints]` / `[workspace.lints]` |
| Same `[lints]` block copied into every member | `[workspace.lints]`, and `lints.workspace = true` |
| `pedantic = "warn"` alongside single-lint overrides | `pedantic = { level = "warn", priority = -1 }` |
| `restriction = "warn"` or `nursery = "warn"` | Remove the group; name the lints you want |
| `unwrap_used`/`panic`/`indexing_slicing` denied crate-wide | Remove, unless the crate's contract is never-panic |
| `#[allow(clippy::unwrap_used)]` on test modules | `allow-unwrap-in-tests = true` in `clippy.toml` |
| `#[allow(lint)]` with no `reason` | `#[expect(lint, reason = "...")]` |
| `#[allow]` chosen because the linter was not run | `#[expect]`, then run the linter |
| `#![allow(...)]` at crate root for one call site | Move it to the item |
| `unsafe` block outside any module declared to need it | `unsafe_code = "deny"`, plus an `expect` on the one module |
| CI runs Clippy without `-D warnings`, or without `--all-targets` | Add both; tests and examples are code too |
| Features never built in combination | `cargo hack --feature-powerset` |
| Toolchain floats, so new lints break unrelated PRs | Pin it; take new lints on a deliberate bump |

## Review checklist

- Are the levels in the manifest, shared through the workspace?
- Does every group have `priority = -1`, with restriction and nursery cherry-picked?
- Does every `#[allow]` or `#[expect]` have a reason and the smallest scope?
- Is `unsafe` denied by default and allowed only in named modules?
- Do the enabled restriction lints match the crate's panic policy?
- Does CI deny warnings over all targets and all features, and check formatting and docs?

## Anti-patterns

- Enabling `restriction` as a group, then drowning in `#[allow]`.
- Denying `unwrap` everywhere, then writing `.unwrap_or_default()` to dodge it.
- `#[allow]` without a reason, or at crate level for a one-line exception.
- Lint configuration kept in a CI script no developer runs locally.
- A floating toolchain that changes the lint set without a commit.

## Sources

Verified against these on 2026-11-05, with each lint name and `clippy.toml` key
checked by building a probe crate:

- Clippy documentation: lint groups and their default levels; `lint_groups_priority`.
- Cargo reference: the `[lints]` section (MSRV 1.74, `priority`); workspace lint inheritance.
- The Rust Reference: lint check attributes (`expect`, `reason`).
- Microsoft Pragmatic Rust Guidelines: M-STATIC-VERIFICATION, M-LINT-OVERRIDE-EXPECT, M-CARGO-WORKSPACE.
- Embark Studios standard lints (v6).
- Google Rust Style Guide: "Unsafe code" (isolate unsafe; `SAFETY:` comments).
- The Rust Performance Book: Linting (`disallowed_types`).
