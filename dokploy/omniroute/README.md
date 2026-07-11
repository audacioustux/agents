# OmniRoute on Dokploy

Dokploy Compose deployment for OmniRoute using the upstream web image plus
Redis.

## Files

- `compose.yml` — Dokploy Compose definition.
- `.env.example` — non-secret runtime defaults; copy to `.env` or paste into
  Dokploy environment variables.
- `.github/omniroute/combos.yml` — declarative combo model lists for the single
  `omni.tux.bd` OmniRoute instance.
- `.github/scripts/sync-omniroute-combos.ts` — Deno/TypeScript sync script used
  by GitHub Actions for validation, dry-runs, and applies.

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

Combo configuration is managed in `.github/omniroute/combos.yml`. The file
declares the `https://omni.tux.bd` base URL and each combo's ordered model list.

Merges to `main` run `.github/workflows/omniroute-combos.yml` with Deno. The
workflow reads the `OMNIROUTE_API_KEY` secret from the `omni.tux.bd` GitHub
Environment, runs the script tests, dry-runs the diff, then applies changed
combo model lists through OmniRoute `/api/combos`.

The sync only manages each combo's ordered `models` list. It preserves live
combo metadata and retained model-entry metadata, including weighted-combo
weights. It never creates or deletes combos; unexpected live combo names or
missing declared combos fail validation instead.

Run the same checks locally with:

```bash
deno test --allow-read --allow-net .github/scripts/sync-omniroute-combos.ts
deno run --allow-read --allow-net --allow-env=OMNIROUTE_API_KEY .github/scripts/sync-omniroute-combos.ts --dry-run
```
