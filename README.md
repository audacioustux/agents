# agents

A curated corpus of agent skills, the declarative spec for the OmniRoute router
that powers the workspace's chat surface, and the Compose stacks that run the
runner pool and memory backend. It is **not** an application repository — sibling
repos (e.g. `TheGrid`, `Tickify`) install the corpus from this tree and reconcile
their `skills-lock.json` against `provenance.json`.

## What this tree holds

| Path | What it is | Start here |
| --- | --- | --- |
| `.agents/skills/` | The skill corpus. Each skill is a directory with a `SKILL.md` frontmatter entry point and optional `references/`, `prompts/`, `scripts/`, `examples/`. | [`.agents/skills/README.md`](.agents/skills/README.md) |
| `.omp/` | Runtime agent config: provider, enabled models, model roles, retry chains, the `oracle` read-only consultant, and the reviewer-gate hook rule. | [`.omp/agent/config.yml`](.omp/agent/config.yml), [`.omp/agent/agents/oracle.md`](.omp/agent/agents/oracle.md) |
| `.github/workflows/omniroute-combos.yml` | CI that applies `dokploy/runner-web/settings/combos.yml` to the live router on every push to `main`. | the workflow file; tests at [`.github/scripts/sync-omniroute-combos.test.ts`](.github/scripts/sync-omniroute-combos.test.ts) |
| `dokploy/runner-web/` | The OmniRoute stack (web + Redis). | [`dokploy/runner-web/README.md`](dokploy/runner-web/README.md) |
| `dokploy/github-runner/` | Self-hosted GitHub Actions runner pool. One pool today (`runner-tickify`); the README records why a TheGrid pool was retired. | [`dokploy/github-runner/README.md`](dokploy/github-runner/README.md) |
| `dokploy/hindsight/` | The memory backend (`vectorize-io/hindsight`). Single container, embedded `pg0`. | [`dokploy/hindsight/compose.yml`](dokploy/hindsight/compose.yml) |
| `docs/audits/` | Worked-example audits of upstream skill registries. The cursor/plugins audit is the model every new audit follows. | [`docs/audits/cursor-plugins/README.md`](docs/audits/cursor-plugins/README.md) |
| `provenance.json` | Maintenance ledger. Per-skill source repos and pinned SHAs, per-file SHA-256s, disposition notes, upstream status. **Maintenance metadata only — never loaded by agents.** | the file's own `note`, `upstreamRetrieval`, and `lockfileNotes` fields |

## What does not belong here

App code. Vendor-tool skills. Anything that has a publisher of its own.

The corpus admits only **cross-cutting craft knowledge** — accessibility,
review, debugging, testing, layout, color, typography, copy, repository
conventions. The rule and the table of what to install from a vendor instead
live at [`.agents/skills/README.md`](.agents/skills/README.md) under "What
belongs here." This README does not restate them.

## How work happens here

The repo operates under a small set of rules. Each rule is enforced or owned
by a single file; this section names them and points at the owner.

| Rule | Owner |
| --- | --- |
| Every skill earns its place through a documented audit with dispositions, evidence, and landed-in SHAs. | [`docs/audits/`](docs/audits/) (worked example: cursor/plugins) |
| Each skill lives at exactly one home; siblings defer rather than restate. | [`.agents/skills/README.md`](.agents/skills/README.md) |
| Upstream bytes are fetched on demand at the pinned SHA, not vendored; the recorded hash verifies drift on re-merge. | `provenance.json` → `upstreamRetrieval` |
| A skill folder's hash covers every file in the directory, not `SKILL.md` alone. | `provenance.json` → `lockfileNotes` |
| Adding, removing, rewriting, or retiring a skill updates `provenance.json` in the same commit. | `provenance.json` (its own `note`) |
| A language- or stack-specific skill is admitted only as a **bridge** — every pattern it carries is named as the expression of a cross-stack rule owned elsewhere. Stack-only rules belong in the consumer repo. | [`.agents/skills/README.md`](.agents/skills/README.md) under "What belongs here"; example bridge: [`typescript-type-discipline`](.agents/skills/typescript-type-discipline/SKILL.md) |
| Major work gets a reviewer pass before the edit and another over the committed diff. A green gate is not a review. | [`.omp/rules/reviewer-gate-around-commits.md`](.omp/rules/reviewer-gate-around-commits.md) |
| Completion claims are evidence, not assertion; run the command and read the output before saying it passed. | [`.agents/skills/verifying-before-completion/SKILL.md`](.agents/skills/verifying-before-completion/SKILL.md) |
| Router combos are declarative. Edits to `dokploy/runner-web/settings/combos.yml` reach the live router through the CI workflow, never by hand. | [`.github/workflows/omniroute-combos.yml`](.github/workflows/omniroute-combos.yml), [`dokploy/runner-web/README.md`](dokploy/runner-web/README.md) § Combo sync |

## Adding or changing a skill

The procedure — what counts as portable, when an audit is required, how to
record provenance, how to close loopholes — lives at
[`.agents/skills/README.md`](.agents/skills/README.md) under "Adding or changing
a skill" and [`.agents/skills/writing-skills/SKILL.md`](.agents/skills/writing-skills/SKILL.md)
under its TDD-for-documentation model. Read both before writing or merging a
skill.
