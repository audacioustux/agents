# Audit: Spielewoy/autoprompt-skill

Source `Spielewoy/autoprompt-skill` at `<pinned SHA>`, audited 2026-09-21.

Every contract in `agents/contracts/` and every generated package in `agents/providers/` — 6 entries — carries exactly one disposition in
[`2026-09-21-dispositions.json`](./2026-09-21-dispositions.json). The ledger's `total` field asserts 11, reflecting the 11 provider packages; the contracts themselves are inventoried under their own rows.

## Result

| Verdict | Count |
|---|---|
| reject | 3 |
| adopt-rule | 3 |
| duplicate | 0 |
| product-specific | 0 |
| adopt-skill | 0 |

All entries are `no-action` pending further investigation; the audit is **incomplete** because the four candidate `adopt-rule` rows require full reads of `routes.json`, `gates.json` + `state-machine.json`, and the conformance-evidence JSONs before they can land.

| From | Into | Status |
|---|---|---|
| `contracts/routes.json` (no-fallback-route) | unresolved (verifying-before-completion vs new route-classification skill) | needs full read |
| `contracts/gates.json` + `state-machine.json` (freeze-before-check + non-resetting retry) | verifying-before-completion | needs full read |
| `contracts/codex-live-conformance-evidence.json` + sibling (conformance-evidence refusal) | harness-bridge | needs full read |
| `contracts/personas/` | — | reject (overlaps with existing skills) |
| `providers/*` (11 generated packages) | — | reject (generation pipeline the corpus does not have) |
| `contracts/README.md` (architecture framing) | — | reject (packaging model the corpus does not use) |

## Fork decision: NO

**Ground: generation coupling.** Forking means adopting the generator script + 11 output trees with machine-owned compiled-block markers. The corpus has no compiler pipeline; adopting one would invert the audit model (curated skills become generated projections). `provenance.json` already rejects vendor-tool skills on staleness/maintenance grounds.

The prior round framed the no-fork argument as "550 hand-authored artifacts" and "vendor paths." Both were overcounts or misattributions:

- The 550 count was wrong: the real structure is 7 hand-authored contracts + 11 generated provider packages + manifests, not 550 hand-authored artifacts.
- "vendor paths" is conditional in `provenance.json` ("when the repo declares one"); the no-fork argument does not depend on it.

## What changed since round 1

A first-pass verdict (drafted by puku-cli, not the corpus) blended two orthogonal decisions — "fork?" and "extract?" — into one statement. omp, reviewing under the harness-bridge safety contract (read-only, approval-gated), surfaced the conflation and the missing audit schema. The revised verdict uses the corpus's documented `dispositions` format from `docs/audits/cursor-plugins/README.md:47-50`, names each contract by its verified path, and records caveats for every row that needs further reading.

## Reading a row

```
disposition  adopt-skill | adopt-rule | duplicate | reject | product-specific
status       landed | no-action
note         why this verdict
evidence     how the file was assessed
landedIn     commit sha plus destination; present iff status is landed
caveat       conditions that would change the verdict; present on adopt-rule rows
```

`note` states the verdict and its ground: a `duplicate` names the corpus skill that
already owns the rule, an `adopt-rule` names its destination, a `reject` gives the
reason. `evidence` records how the file was actually read — a full-body read, a
targeted grep, or a RED baseline — because two verdicts were overturned when that
distinction was checked (see `docs/audits/cursor-plugins/README.md:Notes on method`).

`caveat` exists because roughly 20% of `adopt-rule` recommendations do not survive
scrutiny as stated. Every adopt-rule row in this ledger carries one.

## Next steps

1. Read `routes.json` in full to resolve the destination for the no-fallback-route rule.
2. Read `gates.json` AND `state-machine.json` in full to specify what "failure" means under non-resetting retry limits.
3. Read `codex-live-conformance-evidence.json` to confirm the conformance-evidence rule generalises beyond stream-json and meets the harness-bridge bar.
4. If all three resolve to clear adopt-rule destinations, file 3 PRs and update this ledger with `landedIn` SHAs.
