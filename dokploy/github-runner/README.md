# GitHub Actions runners on Dokploy

Dokploy Compose deployment for self-hosted GitHub Actions runner pools
(`myoung34/github-runner`). Self-hosted runner minutes are free on every
GitHub plan; jobs run here instead of drawing on the hosted-runner quota.

## Pools

| Service | Scope | Label | Replicas |
|---|---|---|---|
| `runner-tickify-nonprod` | repo `stage-crew/tickify` | `dokploy-tickify-nonprod`, `dokploy-tickify` | 2 (10 CPUs each) |
| `runner-tickify` | repo `stage-crew/tickify` | `dokploy-tickify` only | 1 (2 CPUs, 7 GB) |
| `runner-optics` | repo `nobinalo/Optics` | `dokploy-optics` | 2 (4 CPUs each, idle cores only) |

A runner registers to exactly one scope, so each pool is its own service;
scaling is its `deploy.replicas`. A TheGrid pool was tried and retired:
that workspace's kernel-adjacent surface (io_uring, O_DIRECT, seccomp
prctl) keeps colliding with container confinement, so its CI stays on
hosted runners. Add pools only for repos whose jobs are container-clean.

## Files

- `compose.yml` — Dokploy Compose definition.
- `.env.example` — required secrets; paste into Dokploy environment variables.

## Deploy

1. In Dokploy, create a project and add a Compose service.
2. Point the service at this repo path: `dokploy/github-runner`.
3. Use `compose.yml` as the Compose file.
4. Set the variables from `.env.example` in Dokploy env, every one of them:
   a missing token fails the whole deploy, and the running pools stay as
   they are. Each pool reads `<POOL>_REPO_URL` and `<POOL>_ACCESS_TOKEN`
   (`TICKIFY_…`, `OPTICS_…`). A deployment set up before these names has
   tickify's as `REPO_URL` and `ACCESS_TOKEN`: rename them
   `TICKIFY_REPO_URL` and `TICKIFY_ACCESS_TOKEN`, and add
   `OPTICS_REPO_URL` and `OPTICS_ACCESS_TOKEN`, before redeploying.
5. Deploy. Runners appear under each repo's Settings → Actions → Runners,
   named `dokploy-tickify-nonprod-…`, `dokploy-tickify-…` and
   `dokploy-optics-…`.

## Runtime

- Image: `myoung34/github-runner:ubuntu-noble`; one replica = one concurrent
  job.
- Docker (tickify only): host socket mounted and host networking —
  workflow `services:` containers (Tickify's CI runs postgres:18 and
  redis:8) run as siblings on the host daemon. Optics' jobs start no
  containers, so its pool has neither, and runs on the stack's own network.
- Secrets: each pool is given only its own repo's token. The image
  un-exports `ACCESS_TOKEN`, but any other variable reaches every job, so a
  shared `.env` would hand each pool's `<POOL>_ACCESS_TOKEN` to the other's
  jobs. A pool's own token stays readable by its root jobs
  (`/proc/1/environ`).
- CPU: tickify's two replicas hold the host's 20 cores. Optics' replicas
  borrow idle ones at a small weight (`cpu_shares: 64` each against
  tickify's default): with all four busy, Optics gets roughly 0.6-2.1 cores
  depending on the host's cgroup version, and tickify keeps 89-97%. If
  Optics' jobs time out while tickify is busy, raise that weight.
- Persistent runners, container-local state: each replica self-registers on
  start and deregisters on graceful stop (the 60 s grace period exists for
  that trap), so redeploys don't leak offline duplicates. `/nix` stays warm
  for the replica's lifetime and is cold after a redeploy — a shared volume
  is off the table because concurrent replicas would race the store's
  sqlite db.

## Pointing workflows at a pool

Tickify: `runs-on: dokploy-tickify-nonprod` for every job but production's,
`runs-on: dokploy-tickify` for production's. GitHub has no job priority, so
production's own runner (step 3 below) is what keeps a production deploy
from queueing behind CI's shards: no CI job requests its one label.
Production's jobs may also take an idle `-nonprod` runner, which carries
both. Keep hosted and
self-hosted interchangeable by reverting that one line to `ubuntu-latest`.

Moving tickify to these labels, in this order, so that no job is handed to
a runner that cannot run it:

1. Deploy PR #16's intermediate compose, not this commit, while no CI run is
   in progress (renaming the service replaces the old runners): the pool carries
   both labels, so every tickify job keeps running as before. This compose also
   starts `runner-tickify`, which must not exist while CI still requests
   `dokploy-tickify` (step 3).
2. Land tickify's change of its `runs-on` lines (`-nonprod` for everything
   but production's jobs).
3. Then deploy production's own runner, `runner-tickify` (this compose's
   third service). It has no Docker socket and no host network, which
   tickify's CI needs for its service containers, so it must not exist while
   CI still requests `dokploy-tickify`.

Optics chooses with a repository variable instead:
`runs-on: ${{ vars.CHECK_RUNNER || 'ubuntu-latest' }}`. Set `CHECK_RUNNER`
to `dokploy-optics` once its runners show as online, and unset it to go
back to hosted runners. A job sent to a pool with no runner online waits in
the queue rather than falling back.
