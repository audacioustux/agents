# agents

A curated corpus of agent skills, the declarative spec for the OmniRoute router,
and the Compose stacks for the runner pool and memory backend. Not an
application repository.

## What this tree holds

| Path | Start here |
| --- | --- |
| `.agents/skills/` | [`.agents/skills/README.md`](.agents/skills/README.md) |
| `.omp/` | [`.omp/agent/config.yml`](.omp/agent/config.yml) |
| `.github/workflows/omniroute-combos.yml` | the workflow file; tests at [`.github/scripts/sync-omniroute-combos.test.ts`](.github/scripts/sync-omniroute-combos.test.ts) |
| `dokploy/runner-web/` | [`dokploy/runner-web/README.md`](dokploy/runner-web/README.md) |
| `dokploy/github-runner/` | [`dokploy/github-runner/README.md`](dokploy/github-runner/README.md) |
| `dokploy/hindsight/` | [`dokploy/hindsight/compose.yml`](dokploy/hindsight/compose.yml) |
| `docs/audits/` | [`docs/audits/cursor-plugins/README.md`](docs/audits/cursor-plugins/README.md) |
| `provenance.json` | its own `note`, `upstreamRetrieval`, and `lockfileNotes` fields |

## What does not belong here

App code. Vendor-tool skills. Anything that has a publisher of its own. The
admission rule and the table of what to install from a vendor instead live at
[`.agents/skills/README.md`](.agents/skills/README.md) under "What belongs
here." This README does not restate them.

## How work happens here

Each rule is enforced or owned by a single file. The owner column points at
that file; the rule itself lives there.

| Rule | Owner |
| --- | --- |
| Every skill earns its place through a documented audit. | [`docs/audits/`](docs/audits/) (worked example: cursor/plugins) |
| Each skill lives at exactly one home; siblings defer. | [`.agents/skills/README.md`](.agents/skills/README.md) |
| Upstream bytes are fetched on demand at the pinned SHA. | `provenance.json` → `upstreamRetrieval` |
| A skill folder's hash covers every file in the directory. | `provenance.json` → `lockfileNotes` |
| Adding, removing, rewriting, or retiring a skill updates `provenance.json` in the same commit. | `provenance.json` (its own `note`) |
| A language- or stack-specific skill is admitted only as a **bridge**. | [`.agents/skills/README.md`](.agents/skills/README.md) under "What belongs here"; example bridge: [`typescript-type-discipline`](.agents/skills/typescript-type-discipline/SKILL.md) |
| Major work gets a reviewer pass before the edit and another over the committed diff. | [`.omp/rules/reviewer-gate-around-commits.md`](.omp/rules/reviewer-gate-around-commits.md) |
| Completion claims are evidence, not assertion. | [`.agents/skills/verifying-before-completion/SKILL.md`](.agents/skills/verifying-before-completion/SKILL.md) |
| Router combos are declarative; CI applies them. | [`.github/workflows/omniroute-combos.yml`](.github/workflows/omniroute-combos.yml) |

## Adding or changing a skill

The procedure lives at [`.agents/skills/README.md`](.agents/skills/README.md)
under "Adding or changing a skill" and
[`.agents/skills/writing-skills/SKILL.md`](.agents/skills/writing-skills/SKILL.md)
under its TDD-for-documentation model.
