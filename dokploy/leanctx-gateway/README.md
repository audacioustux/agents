# LeanCTX on Dokploy

Dokploy Compose deployment for two independent LeanCTX planes, both fronting
`omni.tux.bd` / this repo's own content:

- **Wire path** (`gateway` service) — `lean-ctx gateway serve`, a multi-provider
  model reverse proxy in front of `omni.tux.bd` (OpenAI/Anthropic/Gemini-shaped
  traffic, gateway keys, Postgres usage/budget metering, admin console).
- **Read path** (`team` service) — `lean-ctx team serve`, the shared/multi-tenant
  context/index server exposing `ctx_*` tools over MCP + REST/SSE for the repos
  mounted at `/srv/repos`. Never touches model traffic.

## Files

- `compose.yml` — Dokploy Compose definition (postgres + gateway + team).
- `Dockerfile` — one image, runs both `gateway serve` and `team serve`.
- `install-leanctx.sh` — downloads and SHA-verifies the official release binary.
- `setup-image.sh` — installs OS packages (incl. `git`, needed by `team-sync.sh`),
  LeanCTX, and the non-root user.
- `team-sync.sh` — baked into the image; wraps `lean-ctx team sync` with an
  actual `git fetch` + `reset --hard` per workspace (see Scheduled sync below).
- `team.json` — read-path config: workspaces, token hashes, allowed hosts.
- `repos.conf.example` — copy to `repos.conf` (gitignored) with real git URLs.
- `.env.example` — non-secret variable names only; keep real values in Dokploy
  environment variables.

## Dokploy configuration

Use the existing `leanctx-gateway` Compose service.

- Repository: `https://github.com/audacioustux/agents`
- Branch: `main`
- Compose path: `dokploy/leanctx-gateway/compose.yml`
- Build context: `dokploy/leanctx-gateway`

Configure these in Dokploy env; keep secret values out of Git:

- `LEANCTX_VERSION`
- `LEAN_CTX_PROXY_TOKEN`
- `LEAN_CTX_GATEWAY_ADMIN_TOKEN`
- `POSTGRES_PASSWORD`
- `DATABASE_URL`
- `OMNIROUTE_API_KEY`

Runtime config/key material is tracked in this repo because it does not contain
plaintext provider keys or plaintext gateway keys:

- `config.toml` references `OMNIROUTE_API_KEY` by environment variable name.
- `gateway-keys.toml` stores SHA-256 hashes only; plaintext `gk-...` keys are
  never committed.

The compose file mounts them read-only into the container:

```yaml
./config.toml:/etc/lean-ctx/config.toml:ro
./gateway-keys.toml:/etc/lean-ctx/gateway-keys.toml:ro
```

The image sets:

- `LEAN_CTX_CONFIG_DIR=/etc/lean-ctx`
- `LEAN_CTX_GATEWAY_KEYS=/etc/lean-ctx/gateway-keys.toml`

## Public routes

Dokploy domains are managed in Dokploy, not in this compose file.

Current intended routes:

- `lean.tux.bd` -> gateway service port `8484`
- `lean-admin.tux.bd` -> gateway service port `8485` (keep internal-only; do
  not point a Dokploy domain at it)
- `team.tux.bd` -> team service port `8686`

Do not add raw Docker `ports:` mappings for either service.

## One-time setup (per new workspace)

Do this **before the first `docker compose up`** — if `repos.conf` doesn't
exist yet, Docker turns `./repos.conf:/etc/lean-ctx/repos.conf:ro` into an
empty directory instead of failing, and `team-sync.sh` then silently reads
zero workspaces.

1. Copy `repos.conf.example` to `repos.conf`, fill in real git URLs/branches.
2. Copy the corresponding entry into `team.json`'s `workspaces[]` (`id`/`label`/
   `root: /srv/repos/<id>`), matching `repos.conf`'s `<id>`.
3. Compute a token hash: `printf '%s' 'your-plaintext-token' | sha256sum`,
   drop the hash into `team.json`'s `tokens[].sha256Hex`. Never store the
   plaintext token anywhere but the client.
4. `docker exec` into the running `team` container once to seed the clones:
   `team-sync` (reads `repos.conf`, clones anything not already present).

## Scheduled sync (Dokploy Schedule, not a sidecar)

`lean-ctx team sync` alone only runs `git fetch --all --prune` per workspace —
it never advances the checked-out tree `team serve` reads from. Use the
baked-in `team-sync` wrapper instead, which does fetch + `reset --hard` +
`clean -fdx` per `repos.conf` entry before delegating to `lean-ctx team sync`.

Dokploy -> `team` service -> Schedules tab:

- Type: Compose, Service: `team`, Shell: `sh`
- Cron: `*/15 * * * *`
- Command: `team-sync`

Use "Run Now" to test, then confirm via `docker exec ... git -C /srv/repos/api
log -1` that the tree actually advanced after a real upstream commit.

## Runtime chain

```text
OMP  -> https://lean.tux.bd/providers/omniroute/v1 -> gateway -> https://omni.tux.bd
Agent (MCP client) -> https://team.tux.bd/v1/tools/call -> team serve -> /srv/repos/*
```
