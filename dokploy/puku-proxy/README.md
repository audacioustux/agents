# puku-proxy on Dokploy

OpenAI-compatible HTTP proxy over `puku-agent-sdk`. Translates
`POST /v1/chat/completions`, `GET /v1/models`, and `GET /healthz` into
`puku-agent-sdk` `query()` calls, with bearer-token auth at the proxy
boundary.

Source: [audacioustux/puku-proxy](https://github.com/audacioustux/puku-proxy) (private).

## Files

- `compose.yml` — Dokploy Compose definition. Build pulls the source repo
  and builds the image from its `Dockerfile`.
- `.env.example` — non-secret runtime defaults with placeholders. Copy to
  `.env` on the Dokploy host and fill in real values, or paste the
  variables into Dokploy's environment UI.

## Deploy

1. On the Dokploy host, ensure `audacioustux/agents` is checked out (wherever
   Dokploy's compose deploy root lives — typically `/etc/dokploy/...`).
2. `git pull` to pick up this folder.
3. Either:
   - `cp dokploy/puku-proxy/.env.example dokploy/puku-proxy/.env` and edit
     `.env` with the real bearer token, **or**
   - Paste the same variables into Dokploy's per-service environment UI.
4. In Dokploy, create a new Compose service, point it at
   `dokploy/puku-proxy`, and deploy.

The image is built from source on each deploy (Dokploy's build cache keeps
it fast).

## Runtime

- Image source: `audacioustux/puku-proxy` Dockerfile, built in-place.
- App container: `puku-proxy`.
- Network: external `dokploy-network` (shared with `omniroute`, the runners).
- Healthcheck: `GET /healthz` via Bun's `fetch` (no curl in the image).
- Logs: JSON-file driver, 10m rotation, 3 files retained.

## Build cache gotcha

Dokploy's "Clear Build Cache" button in the service UI purges dangling
images but does **not** invalidate BuildKit's layer cache. If a build
keeps replaying an old failed layer (e.g. `npm: not found` from a previous
`-slim` base image), bump the `CACHEBUST` build arg in `compose.yml`:

```yaml
build:
  args:
    - CACHEBUST=deploy-<ISO-timestamp>
```

The arg is consumed by a `RUN echo` at the top of the runtime stage, which
changes the layer hash and forces every layer below it to rebuild. Without
this, changes to the runtime base image (e.g. swapping `-slim` for the
full image) may be invisible to BuildKit until something else invalidates
the chain.

## Auth

Two independent layers:

1. **Proxy bearer** (`PUKU_PROXY_AUTH_KEYS`) — required. Every protected
   request must send `Authorization: Bearer <token>`. The proxy refuses to
   start without this. `/healthz` stays open for orchestrator probes.

2. **Upstream** (`PUKU_AI_API_KEY` / `PUKU_AUTH_TOKEN` / `PUKU_BASE_URL`) —
   forwarded to the spawned `puku-cli` subprocess.

   **Known caveat**: as of puku-cli 1.8.56, the CLI rejects env-var API
   keys with `Not logged in · Please run /login`. The proxy passes the
   value through faithfully; the rejection is upstream. Two workarounds:
   - Run `puku-cli auth login` interactively on the Dokploy host, then
     bake the resulting OAuth state into a custom image.
   - Wait for Puku to ship env-var auth upstream and rebuild.

   Until one of those resolves, `POST /v1/chat/completions` returns 500
   with `Not logged in` even though the proxy itself is healthy.

## Required vs optional env vars

| Var                       | Required | Default | Notes |
|---------------------------|----------|---------|-------|
| `PUKU_PROXY_AUTH_KEYS`    | **yes**  | —       | Comma-separated bearer tokens. Proxy fails to start without this. |
| `PORT`                    | no       | `8787`  | Port inside the container. Compose publishes `${PORT:-8787}:8787`. |
| `PUKU_AI_API_KEY`         | no       | —       | Upstream API key for `puku-cli`. CLI may reject env-var keys (see below). |
| `PUKU_AUTH_TOKEN`         | no       | —       | OAuth alternative to API key. |
| `PUKU_BASE_URL`           | no       | upstream default | Override the upstream gateway. |

## Smoke test after deploy

```bash
# Open (no auth)
curl -s https://<host>:8787/healthz

# Auth required
curl -s -H "authorization: Bearer <token>" https://<host>:8787/v1/models
curl -s -H "authorization: Bearer <token>" \
     -H 'content-type: application/json' \
     -d '{"model":"puku-default","messages":[{"role":"user","content":"hi"}]}' \
     https://<host>:8787/v1/chat/completions
```

Or use the bundled script, which exits with distinct codes for each failure
mode (proxy down vs token rejected vs upstream "Not logged in"):

```bash
HOST=https://<host> PORT=8787 TOKEN=pk_live_<token> ./scripts/smoke.sh
```

## Rotation

To rotate the bearer token:

1. Mint a new key at puku.sh admin UI.
2. Add it to `PUKU_PROXY_AUTH_KEYS` (comma-separated — supports multiple
   for zero-downtime rotation). To revoke the old key, remove it from
   the list and redeploy.
3. Redeploy the service.

Tokens are matched with constant-time comparison and never logged.