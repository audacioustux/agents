---
name: verifying-before-completion
description: Use when about to claim work is complete, fixed, or passing, before committing or creating PRs - requires running verification commands and confirming output before making any success claims; evidence before assertions always
---

# Verification Before Completion

## Overview

Claiming work is complete without verification is dishonesty, not efficiency.

**Core principle:** Evidence before claims, always.

**Violating the letter of this rule is violating the spirit of this rule.**

## The Iron Law

```
NO COMPLETION CLAIMS WITHOUT FRESH VERIFICATION EVIDENCE
```

If you haven't run the verification command in this message, you cannot claim it passes.

## The Gate Function

```
BEFORE claiming any status or expressing satisfaction:

1. IDENTIFY: What command proves this claim?
2. RUN: Execute the FULL command (fresh, complete)
3. READ: Full output, check exit code, count failures
4. VERIFY: Does output confirm the claim?
   - If NO: State actual status with evidence
   - If YES: State claim WITH evidence
   - If the measurement itself is unsound: INCONCLUSIVE
5. ONLY THEN: Make the claim

Skip any step = lying, not verifying
```

A run can fail to answer the question rather than answering it either way. No valid
baseline, a probe that cannot observe the thing claimed, a confound between the two
states, an environment that differs from the one in question — each produces output
that looks like a result and is not one. Report that as inconclusive and say what
would settle it. A green that the check could not have turned red is the most
expensive outcome here, because it ends the investigation while the defect survives.

A claim about code the change does not touch rests on one or two load-bearing
facts — "this only drops cache entries that were already dead". Name them, then
say how far each one got: asserted, cited to a real file and line, argued step
by step, run in a script against the real code, or reproduced in the running
system. Anything short of run is unproven, and the word to write is "unproven",
not a paragraph that reads as settled.

Symbol search finds callers; it does not find consumers. It stops at the edge of
a pinned dependency's own source, at a wire format or database column some other
process parses, at a feature flag that selects the path at runtime, and at code
three hops downstream that never names your symbol. A clean grep bounds where
you looked, not what breaks.

## Common Failures

| Claim | Requires | Not Sufficient |
|-------|----------|----------------|
| Tests pass | Test command output: 0 failures | Previous run, "should pass" |
| Linter clean | Linter output: 0 errors | Partial check, extrapolation |
| Build succeeds | Build command: exit 0 | Linter passing, logs look good |
| Bug fixed | Test original symptom: passes | Code changed, assumed fixed |
| Regression test works | Red-green cycle verified | Test passes once |
| Agent completed | VCS diff shows changes | Agent reports "success" |
| Requirements met | Line-by-line checklist | Tests passing |
| Safe for code outside the diff | The one fact its safety rests on, checked by running the real code | A grep of callers; reasoning that reads convincingly |

## Red Flags - STOP

Observable states, not excuses. The excuses and their rebuttals are in
Rationalization Prevention below.

- Using "should", "probably", "seems to"
- Expressing satisfaction before verification ("Great!", "Perfect!", "Done!", etc.)
- About to commit/push/PR without verification
- **ANY wording implying success without having run verification**

## Rationalization Prevention

| Excuse | Reality |
|--------|---------|
| "Should work now" | RUN the verification |
| "I'm confident" | Confidence ≠ evidence |
| "Just this once" | No exceptions |
| "Linter passed" | Linter ≠ compiler |
| "Agent said success" | Verify independently |
| "I'm tired" | Exhaustion ≠ excuse |
| "Partial check is enough" | Partial proves nothing |
| "Different words so rule doesn't apply" | Spirit over letter |

## Key Patterns

**Tests:**
```
✅ [Run test command] [See: 34/34 pass] "All tests pass"
❌ "Should pass now" / "Looks correct"
```

**Regression tests (TDD Red-Green):**
```
✅ Write → Run (pass) → Revert fix → Run (MUST FAIL) → Restore → Run (pass)
❌ "I've written a regression test" (without red-green verification)
```

**Build:**
```
✅ [Run build] [See: exit 0] "Build passes"
❌ "Linter passed" (linter doesn't check compilation)
```

**Requirements:**
```
✅ Re-read plan → Create checklist → Verify each → Report gaps or completion
❌ "Tests pass, phase complete"
```

**Agent delegation:**
```
✅ Agent reports success → Check VCS diff → Verify changes → Report actual state
❌ Trust agent report
```

## Why This Matters

An unverified completion claim is worse than an admitted unknown: it ends the
conversation, so the defect ships and surfaces later with the context gone.
Undefined functions, missing requirements, and silent regressions all pass a
claim and fail a check.

## When To Apply

**ALWAYS before:**
- ANY variation of success/completion claims
- ANY expression of satisfaction
- ANY positive statement about work state
- Committing, PR creation, task completion
- Moving to next task
- Delegating to agents

**Rule applies to:**
- Exact phrases
- Paraphrases and synonyms
- Implications of success
- ANY communication suggesting completion/correctness

## The Bottom Line

**No shortcuts for verification.**

Run the command. Read the output. THEN claim the result.

This is non-negotiable.
