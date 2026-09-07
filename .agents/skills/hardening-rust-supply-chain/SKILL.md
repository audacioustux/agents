---
name: hardening-rust-supply-chain
description: Use when auditing or configuring a Rust project's dependencies — vulnerability and licence policy, lockfile discipline, reproducible builds, and deciding how much unsafe code a project has inherited.
uses:
  - name: reviewing-unsafe-rust
    source: audacioustux/agents
---

# Hardening the Rust Supply Chain

## Overview

A Rust binary is mostly other people's code. The compiler's guarantees say nothing
about whether that code is maintained, licensed compatibly, or currently the subject
of an advisory.

These checks are worth automating precisely because they fail silently: nothing about
a passing build indicates that a dependency was yanked yesterday.

## When to use

Use for:

- adding, updating, or auditing dependencies
- setting up dependency gates in CI
- responding to a security advisory
- assessing how much unsafe code a project has pulled in

Reviewing unsafe code you own belongs to `reviewing-unsafe-rust`.

## Advisories and policy

Two tools cover this ground, and the split matters. `cargo-audit` answers one
question: does anything in `Cargo.lock` have a known advisory in the RustSec
database? `cargo-deny` runs four checks — `advisories`, `bans`, `licenses`,
`sources` — and covers what advisory scanning does not: licences that are
incompatible or absent, crates the project has decided not to depend on, sources
outside the registries it trusts, and the same crate pulled in at several versions
at once.

Run them in CI on a schedule, not only on dependency changes. The answer changes
without the tree changing, which is the whole reason a green build proves nothing
here.

Turn the warnings that matter into failures — but know which ones you can. Not every
finding is a configurable lint level, and assuming otherwise produces a config that
looks strict and is not. Verified against cargo-deny 0.20.2:

| Finding | Default | Configurable? |
| --- | --- | --- |
| Security vulnerability | error | No — always fails |
| Unlicensed crate | error | No — always fails |
| Non-allowed licence | error | No — always fails |
| `unmaintained` | `all` | Scope, not a level: `all`/`workspace`/`transitive`/`none` |
| `yanked` | `warn` | Yes |
| `multiple-versions` | `warn` | Yes |
| `wildcards` | `allow` | Yes |
| `unknown-registry`, `unknown-git` | `warn` | Yes |

That schema moves. `unmaintained` used to be a lint level and is now a scope, and
`vulnerability`, `severity-threshold`, and `unlicensed` were removed outright once
those findings became unconditional errors. Because the config rejects unknown keys,
a stale key is a hard config error rather than a silent no-op — so check the version
before assuming a mismatch here is drift rather than a change on their side.

The two that actually need changing are `multiple-versions` and `wildcards`. A
warning in a green build is a decision deferred indefinitely, and `wildcards`
defaults to `allow`, so it reports nothing at all until set.

Duplicate versions are worth a specific mention. Two versions of one crate in a tree
means two copies compiled in, and where the crate carries global state or a type that
crosses an API boundary, they are not interchangeable — a type from one version will
not satisfy a signature expecting the other, and the error names the same crate on
both sides.

Wildcard requirements deserve denial for the same reason lockfiles exist: they make
the build's inputs a function of when it ran.

## Lockfiles

Commit the lockfile by default, whatever the crate type. The old rule — commit for
binaries, gitignore for libraries — was officially retired in August 2023, and
`cargo new` no longer ignores it for libraries. The current guidance is to commit
unless a project has a specific reason not to.

What changed is the framing, not the underlying fact. A library's lockfile still does
not affect its consumers, who resolve their own; committing it makes the library's own
CI reproducible, which is worth having on its own terms. The cost is merge conflicts
and the false sense that consumers are getting what you tested.

Because a committed lockfile pins what CI tests, pair it with a scheduled job that
resolves fresh, so newly-published breakage surfaces on your schedule rather than a
contributor's.

In CI, resolve with `--locked` rather than merely having the file present, so a build
fails on a manifest that has drifted instead of quietly updating the lock and
continuing. The failure is the point: it means the dependency change gets reviewed
rather than absorbed. `--frozen` additionally forbids network access; `--locked` alone
does not.

## Assessing inherited unsafe

Unsafe code in a dependency is unsafe code in the binary. `cargo-geiger` counts it
across the tree, giving a proportionate sense of where soundness risk actually lives,
which is often not where a team assumes.

Treat the count as a map rather than a score. A crate with substantial unsafe and a
serious audit history is a better dependency than one with none and no maintenance.
The number tells you where to look, not what to conclude.

Where a dependency's unsafe code is load-bearing for something the project relies on,
that is worth knowing before an advisory forces the question.

## Responding to an advisory

Determine reachability before urgency. An advisory against a code path the project
never calls is a different problem from one on its request-handling path, and treating
both as equally urgent trains people to treat neither as urgent.

Prefer a patch release that keeps the API. Where none exists, a maintained fork or a
replacement crate is a larger change than it looks and should be reviewed as one.

Where the fix cannot land immediately, the ignore entry is the only lever, and it is
blunter than people expect. Neither tool supported an expiry as of cargo-deny 0.20.2
and cargo-audit 0.22.2: `cargo-deny`'s ignore
accepts an advisory id and a free-text `reason` and rejects unknown keys outright, so
an `expires` field is a config parse error rather than a silently-ignored convention.
`cargo-audit`'s ignore list is bare ids with nowhere to put metadata at all.

So the expiry has to live somewhere a human will see it. Put the date and the
condition in the `reason` string, and make something outside the tool — a calendar
entry, a tracking issue — responsible for coming back. An ignore with no stated
end condition is how an advisory becomes permanently invisible.

## Detection patterns

| Symptom | Fix |
| --- | --- |
| Advisory scan runs only on dependency changes | Schedule it; advisories arrive without the tree changing |
| `wildcards` never reported anything | It defaults to `allow`; set it to `deny` |
| `multiple-versions` left at its default | Set to `deny`; duplicates compile in twice and their types do not interchange |
| Config sets a lint level for `vulnerability` or `unlicensed` | Remove it; those are unconditional errors with no key |
| `unmaintained` set to `warn` or `deny` | It takes a scope: `all`/`workspace`/`transitive`/`none` |
| `expires` key in an ignore entry | Parse error, not a deferral; put the date in `reason` |
| Ignore entry with no stated end condition | Add one, and own the follow-up outside the tool |
| Lockfile gitignored because it is a library | Commit it; the bin/lib split was retired in 2023 |
| Committed lockfile and nothing resolves fresh | Add a scheduled job that resolves without it |
| CI has the lockfile present but not enforced | Resolve with `--locked` so manifest drift fails the build |
| Dependency judged by its unsafe count alone | Treat the count as a map; maintenance history matters more |

## Review checklist

- Does CI run `cargo-audit` or `cargo-deny advisories` on a schedule, not only on dependency changes?
- Are `multiple-versions` and `wildcards` raised from their defaults to `deny`?
- Is the licence allow-list actually populated, given unlicensed crates already fail?
- Is the lockfile committed, and paired with a scheduled fresh-resolve job?
- Does CI resolve with `--locked`, failing on drift?
- Does every ignore entry state an end condition in its `reason`?

## Anti-patterns

- Configuring a lint level for vulnerabilities or unlicensed crates, which are always errors.
- Assuming `wildcards` is on: it defaults to `allow` and reports nothing until set.
- An `expires` key in an ignore entry, which is a parse error, not a deferral.
- A committed lockfile with nothing ever resolving fresh, so CI tests one pinned set forever.
- An advisory ignored permanently because the fix was inconvenient once.
- Treating a dependency's unsafe count as a quality score.
- Auditing only on dependency changes, when advisories arrive independently.
