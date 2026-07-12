# LeanCTX Gateway on Dokploy

Dokploy Compose deployment for the LeanCTX Team Gateway in front of the
`omni.tux.bd` OmniRoute instance.

## Files

- `compose.yml` — Dokploy Compose definition.
- `Dockerfile` — small image that installs the LeanCTX release selected by the
  Dokploy `LEANCTX_VERSION` build arg.
- `install-leanctx.sh` — downloads and SHA-verifies the official release binary.
- `setup-image.sh` — installs OS packages, LeanCTX, and the non-root user.
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

Keep runtime config/key material in Dokploy file mounts, not Git:

- `config.toml` mounted to `/etc/lean-ctx/config.toml:ro`
- `gateway-keys.toml` mounted to `/etc/lean-ctx/gateway-keys.toml:ro`

The compose file references those mounts through Dokploy's app-level persisted
`files/` directory from the Git compose working directory:

```yaml
../../../files/config.toml:/etc/lean-ctx/config.toml:ro
../../../files/gateway-keys.toml:/etc/lean-ctx/gateway-keys.toml:ro
```

## Public routes

Dokploy domains are managed in Dokploy, not in this compose file.

Current intended routes:

- `lean.tux.bd` -> gateway service port `8484`
- `lean-admin.tux.bd` -> gateway service port `8485`

Do not add raw Docker `ports:` mappings for the gateway.

## Runtime chain

```text
OMP -> https://lean.tux.bd/providers/omniroute/v1
    -> LeanCTX gateway
    -> https://omni.tux.bd
```
