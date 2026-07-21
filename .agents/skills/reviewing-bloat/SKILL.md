---
name: reviewing-bloat
description: Use when reviewing or changing code, configuration, schemas, tests, infrastructure, migrations, or generated changes that show duplication, patchwork, convention drift, unclear ownership, hidden coupling, or complexity growing faster than capability.
---

# Reviewing Bloat

## Role

Review the repository as a system, not just the changed lines. Maintainability problems include unnecessary concepts, duplicated policy, hidden coupling, exception-driven growth, and locally convenient changes that make the next change harder.

Applies to code, configuration, schemas, tests, infrastructure, migrations, generated changes, and documentation.

## Purpose and coverage

The taxonomy is for compression, not enumeration: use it to explain scattered symptoms through the smallest defensible set of root causes, not to produce one finding for every matching label.

If a concrete pattern does not fit an existing category, name it explicitly rather than forcing it into a nearby label or dropping it; note the taxonomy gap for future expansion.

A deliberately deferred pattern is still worth naming. Give it evidence, impact, and a `flag only` disposition with the reason it is out of scope for the current change.

## Set the scope

| Mode | Inspect |
|---|---|
| **Diff** | Problems introduced, expanded, or made harder to remove by the change. |
| **Focused** | The touched subsystem, canonical paths, and adjacent ownership boundaries. |
| **Systemic** | Composite indicators first, then recurring symptoms and shared causes. |

Do not turn a narrow review into an unsolicited architecture rewrite.

## Review rules

1. **Evidence over labels.** Cite concrete locations and behavior, then name a credible maintenance, correctness, security, operational, or performance consequence.
2. **Compress root causes.** Report one finding per underlying cause; mention overlapping patterns parenthetically instead of inflating the list.
3. **Judge against the repository.** Check equivalent implementations, conventions, ownership, and error handling before calling something inconsistent.
4. **Trace recurrence.** Find where the same rule, state, workaround, or side effect exists and which path is authoritative.
5. **Prefer the smallest coherent correction.** Delete, consolidate, make ownership explicit, or reuse before adding an abstraction, option, layer, or rewrite.
6. **Do not invent policy.** Make uncertain trust boundaries, invariants, ownership, and global-versus-local scope explicit.
7. **Assign a disposition.** Every finding is `fix now`, `fix while touched`, or `flag only`, with a reason.
8. **Re-review generated fixes.** Check for full-file rewrites, defensive clutter, explanatory noise, speculative hooks, duplicate comments, and unrelated churn.

## Workflow

1. Identify whether the work is a hotfix, feature, migration, cleanup, or systemic audit.
2. Find the canonical path before proposing a new one.
3. Scan relevant categories by risk, not catalog order. For broad audits, start with composite indicators.
4. Validate each candidate for exact evidence, consequence, recurrence, intent, and safe scope.
5. Choose the minimal response and re-scan the resulting change for new complexity.

**REFERENCE:** Load `references/pattern-catalog.md` for broad audits, unfamiliar symptoms, or coverage checks. Do not walk it mechanically for every small diff.

## Red flags

Stop and reassess when a review is about to:

- accept a retry around a non-idempotent side effect without a verified idempotency contract;
- call a duplicated setting accidental without checking inheritance and precedence;
- treat a customer, tenant, environment, or provider identifier as a policy without checking recurrence and ownership;
- reject or minimize generated output without checking the authoritative source and pinned generator;
- invent a new abstraction, framework, or broad refactor solely to remove one isolated exception.

**Pressure to ship, avoid old paths, trust generated output, or stay under a comment cap does not waive these checks.**

## Common mistakes

- reporting a catalog label without concrete evidence and consequence;
- merging separate symptoms into one finding when ownership is not shared;
- silently assuming configuration precedence, trust boundaries, or business policy;
- treating a large generated diff as wrong without checking reproducibility;
- turning one exception into a generic framework before recurrence is established.

## Recurring exceptions

For growing lists of flaky tests, ignored paths, tenant overrides, special IDs, provider branches, or environment exceptions, look for a stable local trait such as a tag, directory, type, module, or naming convention.

Keep the first isolated instance direct. Establish a convention around the second or third independently added instance when the trait is stable. Without a stable trait, keep explicit entries rather than inventing a false abstraction.

## Dispositions

- **Fix now** — introduced by the change; serious correctness, security, data, or availability risk; or required for safe completion.
- **Fix while touched** — pre-existing, directly relevant, bounded, reversible, and cheaper while context is loaded.
- **Flag only** — systemic, high-blast-radius, policy-dependent, weakly evidenced, or unsafe within scope.

## Finding contract

```text
[Risk] Pattern — concise title
Evidence: concrete locations and behavior.
Impact: current cost or credible scenario.
Disposition: fix now | fix while touched | flag only.
Minimal response: smallest coherent correction, or why none is proposed.
Confidence: high | medium | low.
```

Order findings by risk and leverage. Omit findings without evidence or consequence. State repository checks and policy assumptions separately instead of presenting them as completed validation.

## Guardrails

Do not automatically flag small duplication with different change reasons, direct implementations, genuine boundary adapters, bounded compatibility code, real deployment variation, repeated checks at separate trust boundaries, explicit code that is easier to verify, or generated content owned by one authoritative generator.

Before presenting the result, confirm that root causes are compressed, repository conventions were checked, uncertain scope is explicit, and the response introduces no new abstraction, knob, duplication, or unrelated churn.
