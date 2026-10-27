# GitHub Actions runners on Dokploy

Dokploy Compose deployment for self-hosted GitHub Actions runner pools
(`myoung34/github-runner`). Self-hosted runner minutes are free on every
GitHub plan; jobs run here instead of drawing on the hosted-runner quota.

## Pools

| Service | Scope | Label | Replicas |
|---|---|---|---|
| `runner-tickify-nonprod` | repo `stage-crew/tickify` | `dokploy-tickify-nonprod` | 2 (10 CPUs each) |
| `runner-tickify-light` | repo `stage-crew/tickify` | `dokploy-tickify-light` | 4 (2 CPUs each, low CPU weight) |
| `runner-tickify` | repo `stage-crew/tickify` | `dokploy-tickify` only | 1 (4 CPUs, 8 GB) |
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
   named `dokploy-tickify-nonprod-…`, `dokploy-tickify-light-…`,
   `dokploy-tickify-…` and `dokploy-optics-…`.

## Runtime

- Image: `myoung34/github-runner:ubuntu-noble`; one replica = one concurrent
  job.
- Docker (tickify's big pool only): host socket mounted and host networking —
  workflow `services:` containers (Tickify's CI runs postgres:18 and
  redis:8) run as siblings on the host daemon. Optics' jobs and tickify's
  light jobs start no containers, so those pools have neither, and run on the
  stack's own network.
- Secrets: each pool is given only its own repo's token. The image
  un-exports `ACCESS_TOKEN`, but any other variable reaches every job, so a
  shared `.env` would hand each pool's `<POOL>_ACCESS_TOKEN` to the other's
  jobs. A pool's own token stays readable by its root jobs
  (`/proc/1/environ`).
- CPU: tickify's two replicas hold the host's 20 cores. Optics' replicas
  borrow idle ones at a small weight (`cpu_shares: 64` each against
  tickify's default): with all four busy, Optics gets roughly 0.6-2.1 cores
  depending on the host's cgroup version, and tickify keeps 89-97% (less
  with the light pool busy too: compose.yml works it out). If Optics' jobs
  time out while tickify is busy, raise that weight. Tickify's light replicas
  run at `cpu_shares: 128`, twice that, since they mostly run beside the check.
- Persistent runners, container-local state: each replica self-registers on
  start and deregisters on graceful stop (the 60 s grace period exists for
  that trap), so redeploys don't leak offline duplicates. `/nix` stays warm
  for the replica's lifetime and is cold after a redeploy — a shared volume
  is off the table because concurrent replicas would race the store's
  sqlite db.

## Pointing workflows at a pool

Tickify: `runs-on: dokploy-tickify-nonprod` for the check (CI's shards and
the Dependabot checks), `runs-on: dokploy-tickify-light` for every other job
but production's, `runs-on: dokploy-tickify` for production's. GitHub has no
job priority, so a runner only one kind of job requests is what keeps that
kind from queueing behind another: each label is carried by one service alone,
so no production job can take a CI runner, no CI job can take production's,
and no small job can take a runner a shard needs. A job that requests a label
no runner has waits in the queue rather than falling back. Keep hosted and
self-hosted interchangeable by reverting the `runs-on` line to
`ubuntu-latest`.

**Rollout order**, so nothing waits at any step:

1. Deploy this compose: the light pool comes up. Harmless alone, nobody asks
   for its label yet, and the big pool's definition is unchanged. Redeploy
   while no check is running, in case Dokploy restarts the whole stack.
2. Land stage-crew/tickify's change that moves the small workflows to
   `dokploy-tickify-light`. Do it with the light runners online (Settings →
   Actions → Runners): while none is, those jobs wait, deploys' gates included.
   Branches cut before it keep asking `dokploy-tickify-nonprod` for their small
   jobs, which still runs them on the big runners as today: nothing breaks.

Backwards: put tickify's workflow back before stopping the light pool.

**Watching the first runs** (the light pool's two unmeasured risks):

- Cold `/nix`: each replica starts empty, so its first job that installs nix
  (`docs.yml`, `check-edge.yml`, the `check` jobs of the `deploy-*` plans) pays
  the install and the store restore. Read those jobs' step times on the first
  pull request after step 2 against the same jobs' recent runs on the big pool
  (`gh run view -R stage-crew/tickify <id> --json jobs`); a `docs` or `check`
  job near its 10 or 15 minute cap on a cold replica is the signal to raise the
  cap or warm the replicas with a no-op dispatch after each redeploy. On the
  host, `df /var/lib/docker` and each replica's `/nix` size.
- Semgrep's memory in 6 GB: `security.yml`'s Semgrep job on the first runs. An
  OOM kill shows as exit 137 or "Killed" in its log; `docker stats` on the
  host during the job gives the peak against the 6 GB limit. Semgrep has no
  `--jobs` set and may size its workers by the CPU count the container
  reports, which on the big pool printed the host's 20 where the quota allowed
  10 (the CPU record in tickify's `ci.yml`): if it OOMs, set `--jobs 2` there or raise `memory` here.
- CPU: the first job to hit its `timeout-minutes` while the big pool is busy
  is the signal to raise `cpu_shares`; a shard that times out while the light
  pool is busy, to lower it.

`runner-tickify` is one replica: production's jobs queue behind each other,
never behind CI. It has no Docker socket and no host network, which tickify's
CI needs for its service containers, so a job of CI that requested
`dokploy-tickify` would fail at "Initialize containers" there. Deploy this
compose only once tickify's workflows request `-nonprod` for everything but
production's jobs (tickify's `ci/prod-runner-labels`); until then the CI pool
is the only runner with the bare label, and tickify's CI relies on it.

CPU: the services' limits sum to 40 CPUs on the host's 20 (production 4, CI
2 x 10, Optics 2 x 4, light 4 x 2), which the owner allowed on 20 Oct and,
for more runners in this repo, on 27 Oct ("you may add more runners in
audacioustux/agents"). A limit is a cap and
not a reservation, and a busy host divides CPU by weight: production's runner,
at the default weight beside CI's two replicas, Optics' two at 64 and the
light pool's four at 128, is owed about 5.5 cores, so CI cannot hold it below
its limit of 4. Production's runner
shares the host, the repository and the access token with the CI pool, whose
jobs hold the Docker socket and run as root; that isolation is deferred.

Optics chooses with a repository variable instead:
`runs-on: ${{ vars.CHECK_RUNNER || 'ubuntu-latest' }}`. Set `CHECK_RUNNER`
to `dokploy-optics` once its runners show as online, and unset it to go
back to hosted runners. A job sent to a pool with no runner online waits in
the queue rather than falling back.
