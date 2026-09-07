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

Advisory scanning answers one question: does anything in the tree have a known
vulnerability? Run it in CI rather than on request, because the answer changes without
the tree changing.

Policy checking is the broader gate and covers what advisory scanning does not:
licences that are incompatible or absent, crates the project has decided not to
depend on, sources outside the registries it trusts, and the same crate pulled in at
several versions at once.

Set each policy to deny rather than warn. A warning in a green build is a decision
deferred indefinitely, and the deferral is invisible to whoever ships next.

Duplicate versions are worth a specific mention. Two versions of one crate in a tree
means two copies compiled in, and where the crate carries global state or a type that
crosses an API boundary, they are not interchangeable — a type from one version will
not satisfy a signature expecting the other, and the error names the same crate on
both sides.

Wildcard version requirements deserve denial for the same reason lockfiles exist:
they make the build's inputs a function of when it ran.

## Lockfiles

Commit the lockfile for anything that ships as a binary. It is the record of what was
actually built, and without it a rebuild of the same commit is a different build.

Libraries are the opposite case: their lockfile is not used by consumers, who resolve
their own, so committing it records only what the library's own CI happened to test.

In CI, resolve with the lockfile enforced rather than merely present, so a build fails
on a manifest that has drifted rather than quietly updating the lock and continuing.
The failure is the point: it means the dependency change gets reviewed rather than
absorbed.

## Assessing inherited unsafe

Unsafe code in a dependency is unsafe code in the binary. Counting it across the tree
gives a proportionate sense of where soundness risk actually lives, which is often not
where a team assumes.

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

Where the fix cannot land immediately, record the exception with an expiry rather than
silencing the check. A permanent ignore entry is how an advisory becomes invisible.

## Review checklist

- Does CI run advisory scanning on a schedule, not only on dependency changes?
- Are licence, banned-crate, source, and duplicate-version policies set to deny?
- Are wildcard version requirements denied?
- Is the lockfile committed for binaries, and absent or unused for libraries?
- Does CI resolve with the lockfile enforced, failing on drift?
- Is any advisory exception recorded with an expiry rather than an open-ended ignore?

## Anti-patterns

- Policies set to warn, in a build nobody reads the warnings of.
- A lockfile committed for a library, recording only its own CI's resolution.
- An advisory ignored permanently because the fix was inconvenient once.
- Treating a dependency's unsafe count as a quality score.
- Auditing only on dependency changes, when advisories arrive independently.
