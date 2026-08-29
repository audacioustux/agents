# GitHub Actions runner on Dokploy

Dokploy Compose deployment for a self-hosted GitHub Actions runner
(`myoung34/github-runner`), currently serving `stage-crew/tickify`.
Self-hosted runner minutes are free on every GitHub plan; jobs run here
instead of drawing on the hosted-runner quota.

## Files

- `compose.yml` — Dokploy Compose definition.
- `.env.example` — required secrets; paste into Dokploy environment variables.

## Deploy

1. In Dokploy, create a project and add a Compose service.
2. Point the service at this repo path: `dokploy/github-runner`.
3. Use `compose.yml` as the Compose file.
4. Set `REPO_URL` and `ACCESS_TOKEN` from `.env.example` in Dokploy env.
5. Deploy. The runner appears under the repo's Settings → Actions → Runners
   as `dokploy-…` with label `dokploy`.

## Runtime

- Image: `myoung34/github-runner:ubuntu-noble`, `deploy.replicas: 3` — one
  replica per concurrent job; scaling is that one number.
- Docker: host socket mounted — workflow `services:` containers (Tickify's
  CI runs postgres:18 and redis:8) run as siblings on the host daemon.
- Persistent runner, container-local state: each replica self-registers on
  start and deregisters on graceful stop (the 60 s grace period exists for
  that trap), so redeploys don't leak offline duplicates. `/nix` stays warm
  for the replica's lifetime and is cold after a redeploy — a shared volume
  is off the table because concurrent replicas would race the store's
  sqlite db.

## Pointing workflows at it

In the target repo, change `runs-on: ubuntu-latest` to `runs-on: dokploy`
(the label above). Keep hosted and self-hosted interchangeable by reverting
that one line.
