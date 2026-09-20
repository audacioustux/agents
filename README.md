# agents

A curated corpus of agent skills, the declarative spec for the OmniRoute
router, and the Compose stacks for the runner pool and memory backend. Not an
application repository.

## What does not belong here

App code. Vendor-tool skills. Anything that has a publisher of its own. The
admission rule and the table of what to install from a vendor instead live at
[`.agents/skills/README.md`](.agents/skills/README.md) under "What belongs
here."

## Where to start

- **Working on a skill** — [`.agents/skills/README.md`](.agents/skills/README.md)
- **Writing a skill** — [`.agents/skills/writing-skills/SKILL.md`](.agents/skills/writing-skills/SKILL.md)
- **Auditing an upstream registry** — [`docs/audits/cursor-plugins/`](docs/audits/cursor-plugins/README.md)
- **Maintaining provenance** — `provenance.json`'s own `note`, `upstreamRetrieval`, and `lockfileNotes` fields
- **Configuring the runtime agent** — [`.omp/agent/config.yml`](.omp/agent/config.yml)
- **Standing up a stack** — the `README.md` (or `compose.yml`) at the top of each `dokploy/<stack>/` directory
- **Reviewing before/after a commit** — [`.omp/rules/reviewer-gate-around-commits.md`](.omp/rules/reviewer-gate-around-commits.md)
