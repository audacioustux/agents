# GitHub Actions runners on Dokploy

Dokploy Compose deployment for self-hosted GitHub Actions runner pools
(`myoung34/github-runner`). Self-hosted runner minutes are free on every
GitHub plan; jobs run here instead of drawing on the hosted-runner quota.

## Pools

| Service | Scope | Label | Replicas |
|---|---|---|---|
| `runner-tickify` | repo `stage-crew/tickify` | `dokploy-tickify` | 3 |
| `runner-nobinalo` | org `nobinalo` | `dokploy-nobinalo` | 0 until `NOBINALO_ACCESS_TOKEN` is set |

A runner registers to exactly one scope, so each pool is its own service;
scaling either is its `deploy.replicas`.

## Files

- `compose.yml` — Dokploy Compose definition.
- `.env.example` — required secrets; paste into Dokploy environment variables.

## Deploy

1. In Dokploy, create a project and add a Compose service.
2. Point the service at this repo path: `dokploy/github-runner`.
3. Use `compose.yml` as the Compose file.
4. Set the variables from `.env.example` in Dokploy env.
5. Deploy. Runners appear under Settings → Actions → Runners (repo for
   tickify, org for nobinalo) named `dokploy-tickify-…` / `dokploy-nobinalo-…`.

## Runtime

- Image: `myoung34/github-runner:ubuntu-noble`; one replica = one concurrent
  job.
- Docker: host socket mounted — workflow `services:` containers (Tickify's
  CI runs postgres:18 and redis:8) run as siblings on the host daemon.
- Persistent runners, container-local state: each replica self-registers on
  start and deregisters on graceful stop (the 60 s grace period exists for
  that trap), so redeploys don't leak offline duplicates. `/nix` stays warm
  for the replica's lifetime and is cold after a redeploy — a shared volume
  is off the table because concurrent replicas would race the store's
  sqlite db.

## Pointing workflows at a pool

`runs-on: dokploy-tickify` or `runs-on: dokploy-nobinalo`. Keep hosted and
self-hosted interchangeable by reverting that one line to `ubuntu-latest`.
