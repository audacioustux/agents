# OmniRoute on Dokploy

Dokploy Compose deployment for OmniRoute using the upstream web image plus
Redis.

## Files

- `compose.yml` — Dokploy Compose definition.
- `.env.example` — non-secret runtime defaults; copy to `.env` or paste into
  Dokploy environment variables.
- `settings/combos.yml` — declarative combo model lists for the single
  `omni.tux.bd` OmniRoute instance.
- `.github/scripts/sync-omniroute-combos.ts` — Deno/TypeScript sync script used
  by GitHub Actions for applies.
- `.github/scripts/sync-omniroute-combos.test.ts` — unit tests for the sync script.

## Deploy

1. In Dokploy, create a project and add a Compose service.
2. Point the service at this repo path: `dokploy/runner-web`.
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

Combo configuration is managed in `dokploy/runner-web/settings/combos.yml`. The
file declares the `https://omni.tux.bd` base URL and each combo's declaration,
either as a plain model list:

```yaml
combos:
  coding:
    - cc/claude-sonnet-5
```

or, to also set the combo's strategy, as an object:

```yaml
combos:
  coding:
    strategy: priority
    models:
      - cc/claude-sonnet-5
```

`strategy` only accepts `priority` or `fusion`; other live strategies (e.g.
OmniRoute's default `weighted`) can't be set from here. Omitting `strategy`
leaves the combo's live strategy untouched.

Pushes to `main` run `.github/workflows/omniroute-combos.yml` with Deno. The
workflow reads the `OMNIROUTE_API_KEY` secret from the `omni.tux.bd` GitHub
Environment and applies changed combos through OmniRoute `/api/combos`.

The sync manages each combo's ordered `models` list and, when declared, its
`strategy`. It preserves all other live combo metadata and retained
model-entry metadata, including weighted-combo weights. It never creates or
deletes combos; unexpected live combo names or missing declared combos cause
sync to fail.

Run tests or sync locally with:

```bash
deno test --allow-read --allow-net .github/scripts/sync-omniroute-combos.test.ts
deno run --allow-read=dokploy/runner-web/settings/combos.yml --allow-net --allow-env=OMNIROUTE_API_KEY .github/scripts/sync-omniroute-combos.ts
```
