# OmniRoute on Dokploy

Dokploy Compose deployment for OmniRoute using the upstream web image plus Redis.

## Files

- `compose.yml` — Dokploy Compose definition.
- `.env.example` — non-secret runtime defaults; copy to `.env` or paste into Dokploy environment variables.
- `settings/instances/omni.tux.bd/combos.json` — structured combo source for the `omni.tux.bd` OmniRoute instance.
- `settings/instances/omni.tux.bd/combos.md` — generated combo documentation; do not edit by hand.
- `sync/` — dependency-free Python tooling used by GitHub Actions to validate, render, dry-run, and apply combo updates.

## Deploy

1. In Dokploy, create a project and add a Compose service.
2. Point the service at this repo path: `dokploy/omniroute`.
3. Use `compose.yml` as the Compose file.
4. Copy `.env.example` to `.env` or paste the variables into Dokploy.
5. Ensure the external `dokploy-network` exists.
6. Deploy.

## Runtime

- Image: `diegosouzapw/omniroute:latest-web`
- App container: `omniroute`
- Redis container: `omniroute-redis`
- Data volume: `omniroute-data` mounted at `/app/data`
- Redis volume: `redis-data` mounted at `/data`
- Network: external `dokploy-network`
- Healthcheck: `node healthcheck.mjs`

## Combo sync

Combo configuration is managed per OmniRoute instance under `settings/instances/<environment>/combos.json`. The directory name is also the GitHub Environment name.

For `omni.tux.bd`, edit `settings/instances/omni.tux.bd/combos.json`. Merges to `main` run `.github/workflows/omniroute-combos.yml`, read the `OMNIROUTE_API_KEY` secret from the `omni.tux.bd` GitHub Environment, dry-run the diff, then apply the declared combo changes through OmniRoute `/api/combos`.

Generated Markdown lives beside the JSON as `combos.md` and is checked in CI with:

```bash
python dokploy/omniroute/sync/render_combos_md.py --instance omni.tux.bd --check
```
