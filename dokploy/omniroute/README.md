# OmniRoute on Dokploy

Single-container deployment of the upstream OmniRoute image, mirroring the
Fly deploy in [`../../fly/omniroute`](../../fly/omniroute). Migrated off Fly;
data migration is handled manually (import the old `/app/data`).

## Deploy (Dokploy UI)

1. Create project → **Create Service → Compose**.
2. Point it at this repo path (`dokploy/omniroute`) or paste `docker-compose.yml`.
3. Open the **Environment** tab, paste the contents of `.env.example`, adjust
   secrets and the domain (`omni.tux.bd`).
4. Ensure the `dokploy-network` exists (Dokploy creates it by default).
5. Deploy.

The Traefik labels route `https://omni.tux.bd` → container port `20128`. If
you prefer, remove the labels and add the domain via Dokploy's **Domains** tab
instead (Dokploy injects the labels for you).

## Runtime

- Image: `diegosouzapw/omniroute:3.8.45`
- Port: `20128` (dashboard + API)
- Data volume: `omniroute-data` → `/app/data`
- No Redis: in-memory rate limiting + SQLite quota store (same as Fly).
- Healthcheck: `GET /api/monitoring/health`, expects `status: "healthy"`.

## Migration notes

- If importing the Fly `/app/data`, reuse the **same** `JWT_SECRET`,
  `API_KEY_SECRET`, `STORAGE_ENCRYPTION_KEY`, and `MACHINE_ID_SALT` the Fly
  instance used. Different keys → encrypted provider credentials/OAuth tokens
  won't decrypt. Check `fly secrets list` first.
- Set `BASE_URL` / `NEXT_PUBLIC_BASE_URL` to the new HTTPS domain.
