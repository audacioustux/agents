---
name: weighing-quality-dimensions
description: Use when writing or reviewing a plan, design doc, decision record, or PR that makes a technical decision, and when asked to "check the -ilities" or quality attributes. Also when a change adds a cache, queue, retry, in-memory state, or dependency, or claims to be faster, to scale, to be cheaper, or to survive a failure without a measurement.
uses:
  - name: writing-plans
    source: audacioustux/agents
  - name: designing-idempotent-boundaries
    source: audacioustux/agents
---

# Weighing Quality Dimensions

A decision can be correct on its own terms and still be poor because of what it
does to a dimension nobody looked at. The model is right, the tests pass, and the
cache still serves yesterday's price. So each technical decision is weighed
against the full list: the author states what it moves, the reviewer sweeps all of it.

## The dimensions

Use these names verbatim. A renamed dimension ("Availability / UX", "Operability",
"Simplicity") cannot be checked against the list and hides what it replaced.

1 Correctness; 2 Modularity; 3 Reusability; 4 Scalability; 5 Security;
6 Reliability; 7 Usability; 8 Testability; 9 Extensibility; 10 Performance
Efficiency; 11 Maintainability; 12 Compatibility and Interoperability;
13 Portability and Adaptability; 14 Accessibility and Inclusivity; 15 Privacy and
Data Protection; 16 Data Quality and Integrity; 17 Robustness, Resilience, and
Recoverability; 18 Operational Readiness and Observability; 19 Deployability and
Change Safety; 20 Software Supply-Chain Integrity; 21 Documentation Quality and
Developer Experience; 22 Compliance and Auditability; 23 Cost Efficiency;
24 Environmental Sustainability; 25 Safety and Harm Prevention (where applicable);
26 AI/LLM Safety (where applicable).

25 applies where the system can injure people or damage property. 26 applies where
an LLM's output reaches a user or triggers an action.

## Authoring: list only what the decision moves

A dimension is moved when the decision gives a reader something to act on there:
a test to add, a reader to warn, a cost to budget. Each moved dimension gets one line:

- **direction**: better, worse, or unverified. Unverified means the decision
  may move the dimension and nobody has checked; give the falsifier that would
  settle it. "Mixed" is two lines, and the same number may appear twice.
- **magnitude**: the number, where one was claimed or already measured.
  Otherwise leave it out.
- **evidence**: a falsifier, a measurement with its command, or a citation. A pure
  judgement ("clearer name") may stand if it is labelled as one.

```markdown
## Quality
- 10 Performance Efficiency — better: session lookup 3ms → 0.05ms (local bench,
  `make bench-sessions`); production unmeasured. Gate: `test_session_lookup_p99`.
- 19 Deployability and Change Safety — worse: each rolling deploy drops the
  sessions on every restarted pod; at 4 deploys/day each user is logged out ~4×.
- 5 Security — worse: revocation reaches one pod of 6. Falsifier: revoke, then
  request via another pod; the session is still accepted.
```

Every regression gets a line. Gains are optional, but a gain that is stated needs
evidence like any other line. A length limit cuts prose and
merges lines, but it never drops a moved dimension.

**Leave every other dimension out.** No "N/A" lines, no "not applicable: …" roll
call, no line for a dimension the change leaves as it was. Each one costs every
reader and tells them nothing. Where someone asks to see that the sweep happened,
one sentence does it: "Swept against the 26 dimensions; the rest are unmoved."

## Reviewing: sweep all, report findings only

Walk all 26 against the decision, including the ones the author left out.
Recalling dimensions from memory misses the same ones the author missed; working
through the list forces the ones nobody thought of. Report only the dimensions
that have a finding. A finding is any of:

- a dimension made worse, and the decision does not say so;
- a stated gain or regression with no evidence;
- a measurable claim with no gate;
- a trade-off accepted somewhere other than the decision record.

**Severity follows the impact, not the requester.** A finding blocks when it would
harm users, money, or data before anyone could notice, or when it cannot be undone
(a dropped table, a sent message, a charge). Anything else is non-blocking:
reported, and labelled as non-blocking.

| The decision adds | Sweep at least |
| --- | --- |
| A cache | 1 staleness, invalidation; 6 behaviour when the cache is down; 23 its cost; 18 hit rate |
| A queue | 16 ordering; 1 duplicate delivery; 17 poison messages, backlog; 18 depth and age |
| A retry | 1 duplicates if the callee is not idempotent; 4 load amplification; 10 tail latency |
| In-memory or pod-local state | 17 loss on crash and deploy; 19 rolling deploys; 4 per-instance caps |
| A dependency or service | 20 pinning, provenance; 6 its availability becomes yours; 23 cost; 12 format differences |
| A removed table, field, or key | 12 every reader; 19 rollback to the old binary; 22 the records it held |

To make a retried handler safe, use the `designing-idempotent-boundaries` skill if
it is available. Otherwise, require a key the sender supplies, recorded in the same
transaction as the effect.

## A measurable claim needs a gate

"Faster", "flat in N", "scales to 10×", "cheaper", and "survives a crash" each
become a test with a written threshold, kept in the suite or CI. The test must fail
against the naive baseline (the code before the change, or the obvious
implementation) and pass against the change.

A one-off benchmark is evidence for today, not a gate: nothing stops the next
change from regressing it. Ask for both, the measurement now and the gate to keep
it. Where a gate is infeasible, the author says why in the record, the reviewer may
contest it, and the claim is written as unmeasured. If the `writing-plans` skill is
installed, its "Name the Missing Measurement" section owns that wording. Otherwise,
say what is unmeasured and what would measure it.

## Trade-offs are named where the decision is recorded

An accepted regression goes in the plan, design, or decision record, beside the
decision it comes from. Record what gets worse, by how much, why it is accepted,
who accepted it, and what would reverse it. The author may propose accepting a
regression; whoever owns the affected users, data, or budget accepts it. A trade-off
accepted in a review thread or a chat is lost on the next read.

## Common mistakes

| Symptom | Fix |
| --- | --- |
| A moved dimension filed among the unmoved ("slightly better") | Its own line, with evidence |
| "Better" with "depends on…" as its evidence | It is unmeasured; say so |
| "Load-test before the demo" as the answer to a latency target | Name the test, its threshold, and that it fails on the baseline |
| Findings drawn only from the mechanisms the doc discusses | Sweep the list; check the table for each mechanism added |

## Rationalizations

| Excuse | Reality |
| --- | --- |
| "They're real but not blockers, so I honored his request and skipped them." | "Only flag blockers" decides labels, not scope. A skipped finding ships. |
| "Those are design questions that spec review already settled." | Spec review covered the spec, not this list. Name what it did not sweep. |
| "I grouped them into one line. That's honest that I considered them." | A roll call of names is the N/A list on one line. One sentence, no names. |
| "Rollback is safe … but that is also a silent behavior change." | A silent behavior change is a regression. State it. |
| "Benchmarked locally; production not yet measured." | Fine as evidence, if it says so. It still needs a gate. |
| "I left it out to fit one screen." | Cut prose, not dimensions. A dimension dropped for length is a regression nobody stated. |
| "They asked for all 26, so I listed them." | Say once why the unmoved ones are left out. If a person or a required template still wants the full list, put it in an appendix after the moved lines, never mixed into them. One line of names is enough there. |
