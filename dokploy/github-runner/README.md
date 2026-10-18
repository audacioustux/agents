# GitHub Actions runners on Dokploy

Dokploy Compose deployment for self-hosted GitHub Actions runner pools
(`myoung34/github-runner`). Self-hosted runner minutes are free on every
GitHub plan; jobs run here instead of drawing on the hosted-runner quota.

## Pools

| Service | Scope | Label | Replicas |
|---|---|---|---|
| `runner-tickify-nonprod` | repo `stage-crew/tickify` | `dokploy-tickify-nonprod` | 2 (10 CPUs each) |
| `runner-tickify` | repo `stage-crew/tickify` | `dokploy-tickify` only | one per job, on its own VM, not Dokploy: [`../github-runner-tickify`](../github-runner-tickify/README.md) |
| `runner-optics` | repo `nobinalo/Optics` | `dokploy-optics` | 2 (4 CPUs each, idle cores only) |

A runner registers to exactly one scope, so each pool is its own service;
scaling is its `deploy.replicas`. A TheGrid pool was tried and retired:
that workspace's kernel-adjacent surface (io_uring, O_DIRECT, seccomp
prctl) keeps colliding with container confinement, so its CI stays on
hosted runners. Add pools only for repos whose jobs are container-clean.

## Files

- `compose.yml` — Dokploy Compose definition.
- `register-entrypoint.sh` — registers tickify's runners from the access-token file.
- `.env.example` — required variables; paste into Dokploy environment variables.

## Deploy

1. In Dokploy, create a project and add a Compose service.
2. Point the service at this repo path: `dokploy/github-runner`.
3. Use `compose.yml` as the Compose file.
4. On the host, write tickify's access-token file ("Registration
   credential"), then set the variables from `.env.example` in Dokploy env,
   every one of them: a missing one fails the whole deploy, and the running
   pools stay as they are. Each pool reads `<POOL>_REPO_URL` and
   `<POOL>_ACCESS_TOKEN` (`TICKIFY_…`, `OPTICS_…`), except that tickify's
   token is the file `TICKIFY_ACCESS_TOKEN_FILE` names. A deployment that
   has `TICKIFY_ACCESS_TOKEN` (or the older `ACCESS_TOKEN`): write the token
   to the file, set `TICKIFY_ACCESS_TOKEN_FILE`, and delete the token
   variable from Dokploy, before redeploying.
5. Deploy. Runners appear under each repo's Settings → Actions → Runners,
   named `dokploy-tickify-nonprod-…` and `dokploy-optics-…`.

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
  jobs. Optics' token stays readable by its root jobs (`/proc/1/environ`);
  tickify's is a file ("Registration credential").
- CPU: tickify's two replicas hold the host's 20 cores. Optics' replicas
  borrow idle ones at a small weight (`cpu_shares: 64` each against
  tickify's default): with all four busy, Optics gets roughly 0.6-2.1 cores
  depending on the host's cgroup version, and tickify keeps 89-97%. If
  Optics' jobs time out while tickify is busy, raise that weight.
- Container-local state: each Optics replica self-registers on start and
  deregisters on graceful stop (the 60 s grace period exists for that trap),
  so redeploys don't leak offline duplicates. Each tickify replica registers
  for one job (`EPHEMERAL`), deregisters itself, and the restart policy
  brings it back to register again. `/nix` stays warm for the replica's
  lifetime and is cold after a redeploy — a shared volume is off the table
  because concurrent replicas would race the store's sqlite db. The restart
  keeps the filesystem, so a job's leftovers still reach the next job.

## Registration credential

Tickify's access token needs `administration:write` on the repo: it can add
deploy keys and change an environment's branch rule, so no job should read it.
The image's own options do not keep it from one: `ACCESS_TOKEN` stays in PID
1's environment (`/proc/1/environ`, `docker inspect`), and `UNSET_CONFIG_VARS`
only unsets it inside bash. So the token lives in a root-only file
(`TICKIFY_ACCESS_TOKEN_FILE`, owner root, mode 0400, written over ssh:
Dokploy's file mounts cannot set the mode) mounted as a compose secret;
`register-entrypoint.sh` reads it once and starts the image with the one-hour
registration token alone. It refuses to start if `ACCESS_TOKEN`,
`RUNNER_TOKEN` or `APP_PRIVATE_KEY` is in the environment, if the file is not
root 0400, or if GitHub returns no registration token.

On this host the file is still reachable from a job: CI's jobs run as root
with the Docker socket, so `docker exec` reads anything in a runner container.
The token is out of the environment, not out of the jobs' reach, and a job
holding it can register a runner of its own under any label,
`dokploy-tickify` included.

## Pointing workflows at a pool

Tickify: `runs-on: dokploy-tickify-nonprod` for every job but production's,
`runs-on: dokploy-tickify` for production's. GitHub has no job priority, so
production's own runner is what keeps a production deploy from queueing
behind CI's shards: no CI job requests its one label. Keep hosted and
self-hosted interchangeable by reverting that one line to `ubuntu-latest`.

Production's runner is `../github-runner-tickify`, on a VM of its own. Moving
it there, in this order, so that no job waits on a label no runner carries:
bring up the VM runner and see `dokploy-tickify-…` online beside this host's;
then deploy this compose, which drops this host's production runner and the
CI pool's `dokploy-tickify`; then the canary
(`../github-runner-tickify/README.md`).

Optics chooses with a repository variable instead:
`runs-on: ${{ vars.CHECK_RUNNER || 'ubuntu-latest' }}`. Set `CHECK_RUNNER`
to `dokploy-optics` once its runners show as online, and unset it to go
back to hosted runners. A job sent to a pool with no runner online waits in
the queue rather than falling back.
