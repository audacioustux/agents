# Audit: cursor/plugins

Source `cursor/plugins` at `df3fb154fb98`, audited 2026-09-09.

Every `SKILL.md` in that repository — 91 files, 90 first-party across 15 plugins plus 1 third-party — carries exactly one disposition in
[`2026-09-09-dispositions.json`](./2026-09-09-dispositions.json). Counts are asserted to sum to 91 on every write.

## Result

| Verdict | Count |
|---|---|
| reject | 38 |
| duplicate | 21 |
| adopt-rule | 17 |
| product-specific | 13 |
| adopt-skill | 2 |

19 landed, 72 no-action, zero pending.

## What landed

| From | Into | Commit |
|---|---|---|
| `thermo-nuclear-review` | anchoring guard into requesting-code-review | `13eda09` |
| `no-comments` | cleaning-ai-slop Pass 2, encode-then-delete | `37e08e3` |
| `principle-build-the-lever` | dispatching-parallel-agents, deterministic-lever exclusion | `37e08e3` |
| `principle-separate-before-serializing-shared-state` | dispatching-parallel-agents, partition before serialize | `3b6c3e3` |
| `orchestrate` | dispatching-parallel-agents, Topology section | `f068bda` |
| `make-pr-easy-to-review` | finishing-a-development-branch, history rewrites preserve content | `3b6c3e3` |
| `fix-merge-conflicts` | finishing-a-development-branch, regenerate generated files on conflict | `37e08e3` |
| `verify-this` | INCONCLUSIVE outcome into verifying-before-completion:35,41-46 | `13eda09` |
| `cli-for-agents` | new skill designing-agent-friendly-clis | `13eda09` |
| `control-cli` | new skill verifying-terminal-interfaces | `527dbca` |
| `why` | receiving-code-review, rationale from the record | `3b6c3e3` |
| `interrogate` | requesting-code-review, independent-agreement weighting | `37e08e3` |
| `pr-review-canvas` | requesting-code-review, order the change for reading | `45de0fc` |
| `control-ui` | reviewing-ux-in-browser, role/name/test-id selector discipline | `37e08e3` |
| `architect` | subagent-driven-development, repeated-deviation signal | `3b6c3e3` |
| `reproduce-and-fix-issues` | systematic-debugging/references/four-phases.md, twice-from-reset repro plus the no-injection rule | `37e08e3` |
| `debug-voice` | systematic-debugging/references/four-phases.md, unobservable-failure instrumentation | `f068bda` |
| `principle-test-behavior-not-implementation` | testing-anti-patterns.md, Anti-Pattern 6 | `37e08e3` |
| `blast-radius` | verifying-before-completion, proof ladder and grep bounds | `3b6c3e3` |

## Reading a row

```
disposition  adopt-skill | adopt-rule | duplicate | reject | product-specific
status       landed | no-action
note         why this verdict
evidence     how the file was assessed
landedIn     commit sha plus destination; present iff status is landed
```

`note` states the verdict and its ground: a `duplicate` names the corpus skill that
already owns the rule, an `adopt-rule` names its destination, a `reject` gives the
reason. `evidence` records how the file was actually read — a full-body read, a
targeted grep, or a RED baseline — because two verdicts were overturned when that
distinction was checked.

## Notes on method

Two proposed skills were refused on measurement rather than judgement. Control
agents with no skill loaded were given the tasks each skill claimed to help with,
and satisfied its rules unaided; the sections would have been ceremony. The
refusal notes record both what the baseline demonstrated and what it could not
reach.

Verdicts came from three scout audits of differing strictness, an oracle scope
gate that read the strongest rows independently, and several reviewer rounds. The
ledger's `caveat` field records the disparity between the scouts, because roughly
20% of `adopt-rule` recommendations survived scrutiny as stated.
