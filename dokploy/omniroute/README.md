# OmniRoute on Dokploy

Dokploy Compose deployment for OmniRoute using the upstream web image plus Redis.

## Files

- `compose.yml` — Dokploy Compose definition.
- `.env.example` — non-secret runtime defaults; copy to `.env` or paste into Dokploy environment variables.
- `combo-mapping.md` — live-synced OmniRoute combo inventory, fallback-chain mapping, and configured provider-prefix inventory.

## Deploy

1. In Dokploy, create a project and add a Compose service.
2. Point the service at this repo path: `dokploy/omniroute`.
3. Use `compose.yml` as the Compose file.
4. Copy `.env.example` to `.env` or paste the variables into Dokploy.
5. Ensure the external `dokploy-network` exists.
6. Deploy.

## Environment

Configure secrets and provider credentials in Dokploy or the OmniRoute UI (follow `.env.example`); do not commit `.env`.

## Runtime

- Image: `diegosouzapw/omniroute:latest-web`
- App container: `omniroute`
- Redis container: `omniroute-redis`
- Data volume: `omniroute-data` mounted at `/app/data`
- Redis volume: `redis-data` mounted at `/data`
- Network: external `dokploy-network`
- Healthcheck: `node healthcheck.mjs`

## Combo mapping

`combo-mapping.md` is generated from the live OmniRoute deployment and should be refreshed after combo, fallback-chain, provider, or context-window changes.
