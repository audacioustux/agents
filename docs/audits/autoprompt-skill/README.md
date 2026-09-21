# Audit: Spielewoy/autoprompt-skill

Source `Spielewoy/autoprompt-skill` at `b6516cf52a7891d797621fdd2a8ca0311e1ae0a9`, audited 2026-09-21.

Every contract in `agents/contracts/` — 7 hand-authored files plus the providers/* projection — carries exactly one disposition in
[`2026-09-21-dispositions.json`](./2026-09-21-dispositions.json). The ledger's `total` field sums to 8 disposition rows.

## Result

| Verdict | Count |
|---|---|
| reject | 5 |
| adopt-rule (no-action) | 2 |
| adopt-rule (landed) | 1 |
| duplicate | 0 |
| product-specific | 0 |
| adopt-skill | 0 |

One adopt-rule row has landed in `writing-plans/SKILL.md` (the nonSelectors discipline). The other two candidate `adopt-rule` rows are `no-action` pending full reads.

| From | Into | Status |
|---|---|---|
| `contracts/routes.json` (nonSelectors — what does NOT trigger a plan) | writing-plans | landed |
| `contracts/gates.json` + `state-machine.json` (freeze-before-check + non-resetting retry) | verifying-before-completion | needs full read |
| `contracts/codex-live-conformance-evidence.json` + sibling (conformance-evidence refusal) | harness-bridge | needs full read |
| `contracts/personas/` | — | reject (overlaps with existing skills) |
| `contracts/roles.json` | — | reject (parallel role vocabulary the corpus does not need) |
| `contracts/schemas/` | — | reject (second validation surface; corpus uses frontmatter + type hints) |
| `providers/*` (11 generated packages) | — | reject (generation pipeline the corpus does not have) |
| `contracts/README.md` (architecture framing) | — | reject (packaging model the corpus does not use) |

## Fork decision: NO

**Ground: generation coupling.** Forking means adopting the generator script + 11 output trees with machine-owned compiled-block markers. The corpus has no compiler pipeline; adopting one would invert the audit model (curated skills become generated projections). `provenance.json` decisions.scope already rejects vendor-tool skills on staleness/maintenance grounds.

The prior round framed the no-fork argument as "550 hand-authored artifacts" and "vendor paths." Both were overcounts or misattributions:

- The 550 count was wrong: the real structure is 7 hand-authored contracts + 11 generated provider packages + manifests, not 550 hand-authored artifacts.
- "vendor paths" is conditional in `provenance.json` decisions.paths ("when the repo declares one"); the no-fork argument does not depend on it.

## What changed since round 1

A first-pass verdict (drafted by puku-cli, not the corpus) blended two orthogonal decisions — "fork?" and "extract?" — into one statement. omp, reviewing under the harness-bridge safety contract (approval-gated, not read-only), surfaced the conflation and the missing audit schema. The revised verdict uses the corpus's documented `dispositions` format from `docs/audits/cursor-plugins/README.md § Reading a row`, names each contract by its verified path, and records caveats for every row that needs further reading.

## Reading a row

```
disposition  adopt-skill | adopt-rule | duplicate | reject | product-specific
status       landed | no-action
note         why this verdict
evidence     how the file was assessed
landedIn     commit sha plus destination; present iff status is landed
caveat       conditions that would change the verdict; present on adopt-rule rows
destination  corpus skill or new-skill name; present on adopt-rule rows
```

The first five fields are the cursor-plugins schema (`docs/audits/cursor-plugins/README.md § Reading a row`). The autoprompt-skill ledger extends that schema with two optional fields:

- `caveat` — borrowed from the cursor-plugins *naming convention* (the field is mentioned in their `Notes on method` section even though the README schema block does not list it) because roughly 20% of `adopt-rule` recommendations do not survive scrutiny as stated, so every adopt-rule row carries one.
- `destination` — new in this ledger. The cursor-plugins schema conveys routing through `note` text. This ledger makes the destination a structured field so it can be machine-checked. If `destination` is `unresolved`, the `note` records the discriminator owed.

`note` states the verdict and its ground: a `duplicate` names the corpus skill that already owns the rule, an `adopt-rule` names its destination, a `reject` gives the reason. `evidence` records how the file was actually read — a full-body read, a targeted grep, or a RED baseline — because two verdicts were overturned when that distinction was checked (see `docs/audits/cursor-plugins/README.md:Notes on method`).

The cursor-plugins schema places the auditor on each row as an `audit` field. This ledger collapses that to a single top-level `auditor` because the audit was performed by a single reviewer; future schema readers should not grep for `audit`.

## Reading the summary

The top-level `summary` block records aggregate counts for the disposition map plus two status-overlays on `adopt-rule`:

- `adopt-skill`, `adopt-rule`, `duplicate`, `reject`, `product-specific` — disposition-category counts. Sum equals `total`.
- `landed` — count of rows whose `status` is `landed` (a `status: landed` row is also counted in its disposition category above; `landed` is a status overlay, not a separate bucket).
- `unresolved` — count of `adopt-rule` rows whose `destination` is the literal string `"unresolved"`. These rows have a destination owed but not yet chosen; their `note` records the discriminator. `unresolved` is independent of `landed`: a row can be `landed` only when its destination is named.

Invariant: `adopt-rule` (the category count) equals `landed` (within adopt-rule) + `no-action` (within adopt-rule). When the audit is fully complete, every `no-action` adopt-rule row has either landed (incrementing `landed`) or changed category (e.g., to `reject` because the rule did not survive scrutiny).

## Next steps

1. Read `gates.json` AND `state-machine.json` in full to specify what "failure" means under non-resetting retry limits.
2. Read `codex-live-conformance-evidence.json` to confirm the conformance-evidence rule generalises beyond stream-json and meets the harness-bridge bar.
3. If both resolve to clear adopt-rule destinations, file 2 PRs and update this ledger with `landedIn` SHAs.

## What was adopted from routes.json

The nonSelectors discipline (9 inputs that MUST NOT escalate the route on their own: file-count, repository-size, max-subs, available-agent-count, security-word-alone, refactor-word-alone, research-word-alone, one-failed-implementation, benchmark-label) was adopted into `writing-plans/SKILL.md` as a new "Not by themselves reasons to plan" section. The full routing system (23-input `routeFactsSchema`, 9-rule `precedenceTable`, `routeAnalyst` role with 120s time-limit, `runOwnerDecision` role with 240s time-limit, `capabilityRequirements` matrix) was rejected as inseparable from the autoprompt packaging model.
