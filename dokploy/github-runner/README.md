# GitHub Actions runners on Dokploy

Dokploy Compose deployment for self-hosted GitHub Actions runner pools
(`myoung34/github-runner`). Self-hosted runner minutes are free on every
GitHub plan; jobs run here instead of drawing on the hosted-runner quota.

## Pools

| Service | Scope | Label | Replicas |
|---|---|---|---|
| `runner-tickify-nonprod` | repo `stage-crew/tickify` | `dokploy-tickify-nonprod` | 4 (5 CPUs, 16 GB each) |
| `runner-tickify` | repo `stage-crew/tickify` | `dokploy-tickify` only | 1 (4 CPUs, 8 GB) |
| `runner-tickify-small` | repo `stage-crew/tickify` | `dokploy-tickify-small` only | 2 (2 CPUs, 6 GB each) |
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
   (`TICKIFY_…`, `OPTICS_…`); production's and the small pool share
   `TICKIFY_REPO_URL`, and the small pool has `TICKIFY_SMALL_ACCESS_TOKEN`
   of its own. A deployment set up before these names has
   tickify's as `REPO_URL` and `ACCESS_TOKEN`: rename them
   `TICKIFY_REPO_URL` and `TICKIFY_ACCESS_TOKEN`, and add
   `OPTICS_REPO_URL` and `OPTICS_ACCESS_TOKEN`, before redeploying.
5. Deploy. Runners appear under each repo's Settings → Actions → Runners,
   named `dokploy-tickify-nonprod-…`, `dokploy-tickify-…`,
   `dokploy-tickify-small-…` and `dokploy-optics-…`.

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
- CPU: tickify's four CI replicas hold the host's 20 cores. Optics' replicas
  borrow idle ones at a small weight (`cpu_shares: 64` each against
  tickify's default): with everything busy, Optics gets about 0.2-0.4 cores
  each by the cgroup v1 arithmetic below (the v2 mappings differ; see the
  compose), and a busy-host figure is a calculation, not a measurement. If
  Optics' jobs time out while tickify is busy, raise that weight.
- Persistent runners, container-local state: each replica self-registers on
  start and deregisters on graceful stop (the 60 s grace period exists for
  that trap), so redeploys don't leak offline duplicates. `/nix` stays warm
  for the replica's lifetime and is cold after a redeploy — a shared volume
  is off the table because concurrent replicas would race the store's
  sqlite db.

## Pointing workflows at a pool

Tickify: `runs-on: dokploy-tickify-nonprod` for every job but production's,
`runs-on: dokploy-tickify` for production's. GitHub has no job priority, so a
runner only production requests is what keeps a production deploy from
queueing behind CI's shards: each label is carried by one service alone, so no
production job can take a CI runner and no CI job can take production's. Keep
hosted and self-hosted interchangeable by reverting that one line to
`ubuntu-latest`.

`runner-tickify` is one replica: production's jobs queue behind each other,
never behind CI. It has no Docker socket and no host network, which tickify's
CI needs for its service containers, so a job of CI that requested
`dokploy-tickify` would fail at "Initialize containers" there. Deploy this
compose only once tickify's workflows request `-nonprod` for everything but
production's jobs (tickify's `ci/prod-runner-labels`); until then the CI pool
is the only runner with the bare label, and tickify's CI relies on it.

CPU: the services' limits sum to 36 CPUs on the host's 20 (production 4, CI
4 x 5, small 2 x 2, Optics 2 x 4); the CI limits alone are the host's 20. The
owner allowed going over on 20 Oct. A limit is a cap and not a reservation, and
a busy host divides CPU by weight (cgroup v1 shares; the small pool capped at
its 2 each, the rest divided by weight): production's runner at the default
weight is owed about 3.1 cores beside four CI replicas, below its limit of 4.
So it carries `cpu_shares: 2048`, owed 5.2 before its cap, and CI cannot hold
it below the limit. Production's runner
shares the host, the repository and the access token with the CI pool, whose
jobs hold the Docker socket and run as root; that isolation is deferred.

Optics chooses with a repository variable instead:
`runs-on: ${{ vars.CHECK_RUNNER || 'ubuntu-latest' }}`. Set `CHECK_RUNNER`
to `dokploy-optics` once its runners show as online, and unset it to go
back to hosted runners. A job sent to a pool with no runner online waits in
the queue rather than falling back.

## The small pool

`runner-tickify-small` serves the small jobs of `stage-crew/tickify` (the
security scans, the Edge, backup, e-mail and Grafana checks, `db-backup-freshness`
and the development backup service's deploy), which request
`dokploy-tickify-small` when the repository variable `SMALL_RUNNER` is `on` and
`dokploy-tickify-nonprod` otherwise (`.github/actionlint.yaml` and
`scripts/workflow-conditions.test.ts` in that repo). It is built like
production's runner: the bare label dropped (`NO_DEFAULT_LABELS`), no Docker
socket, no host network (no small job starts a container or reads a
host-published port; `grep` of the eleven workflow files that hold them for `docker `,
`container:`, `services:` and `127.0.0.1` finds none), its own token, no
`env_file`. The backup service's deploy holds Railway and backup credentials, so
this pool is trusted as the CI pool is. 6 GB is more than the hand-run runner's
4 GB because Semgrep is the hungry job and RAM is plentiful; an OOM shows as
exit 137 in a job.

### Moving the small runner in (the hand-run `tickify-runner-small-1`)

Until now the small runner was a container started by hand outside this compose
(same image, `restart: unless-stopped`, host network, 2 CPUs and 4 GB). It
is stopped only after the compose pool is online, because with `SMALL_RUNNER`
on and no small runner registered the small jobs queue until one appears.
`SMALL_RUNNER` stays `on` the whole way; nothing in `stage-crew/tickify` changes.

1. In Dokploy's environment add `TICKIFY_SMALL_ACCESS_TOKEN` (a PAT with the same
   access as `TICKIFY_ACCESS_TOKEN`; the same value works, a separate one can
   be revoked alone). Empty fails the whole deploy and the running pools stay.
2. Merge under the rollout rule below. Dokploy redeploys the stack.
3. Confirm two `dokploy-tickify-small-…` runners are online and idle:
   `gh api repos/stage-crew/tickify/actions/runners --paginate --jq '.runners[] | select(.name|startswith("dokploy-tickify-small")) | [.name,.status,.busy]|@tsv'`.
   The hand-run runner is still listed beside them: that is the overlap, and
   harmless (jobs go to whichever is idle).
4. Only now, with the hand-run runner idle (the same listing, `busy` false):
   `docker stop -t 60 tickify-runner-small-1`. The 60 s lets the image
   deregister it; confirm its name left the listing.
5. Leave the stopped container for a day of green small jobs (`docker start
   tickify-runner-small-1` is the way back), then `docker rm tickify-runner-small-1`.
   Left in place, `unless-stopped` keeps a stopped container stopped across a
   host reboot.

## Four runners of five CPUs: what it costs

The CI limits stay at the host's 20 cores (4 x 5), so this reshapes the pool and
adds no CPU. The host is already saturated (load average 22 / 16 / 13 on 20 cores
at 17:25Z on 3 Nov), so **a shard on a 5-CPU runner is slower than on a 10-CPU
one; nothing is faster by splitting.**

`CHECK_CONCURRENCY=auto` reads the container's CPU quota and runs one unit per
`CORES_PER_UNIT` (2) of it: 5 units on 10 CPUs, 2 on 5
(`scripts/run-check.ts:727`, `.github/workflows/ci.yml:589` in `stage-crew/tickify`
at origin/main `ba4b01864f`). The check's own scheduler replayed on the unit
seconds of run 40826053261 (`scripts/gate-timings.json`: 120 units, 33,588
unit-seconds), longest shard in minutes (a calculation; the seconds do not grow
when a unit gets fewer cores, so the 5-CPU rows are a floor):

| shards x units at a time | runners | longest shard |
|---|---|---|
| 2 x 5 (today, 10 CPUs) | 2 x 10 | 59.8 (59.6 measured) |
| 2 x 2 (this compose alone, `auto`) | 4 x 5 | 144 |
| 4 x 2 (four shards, `auto`) | 4 x 5 | 76 |
| 4 x 3 (four shards, `CHECK_CONCURRENCY=3`) | 4 x 5 | 52 |

The second row is agents#19, closed on 19 Oct: the first shard wholly at 5 CPUs
was cut at the 60-minute step limit (run 39310585455). The third also fails
tickify's own bound: its test holds the step limit to the longest shard plus one
unit limit (23 minutes), 99 minutes against 86. The fourth runs 3 units on 5
CPUs, 1.67 cores each against 2 today. At that density units overlap more: when
the bound went from 4 to 6 on 10 CPUs (1.67 cores per unit, `run-check.ts`'s
`CORES_PER_UNIT` comment), overlapping units ran 1.2-1.5 times slower and the
whole check still got faster, 41.1 to 39.4 minutes, and the replay's 38.0 was
4% off. So expect 52-63 minutes (52 x 1.0-1.2) against 60 now: **the same,
within error.** A cross-check: 33,588 unit-seconds at 2 cores each over 20
cores is 56 minutes whatever the shape, so the host is the limit, not the
pool's layout.

**Should `stage-crew/tickify` deal four shards? Yes, if this lands, and not
otherwise.** Four runners with two shards leave two runners idle for a lone run,
or put two runs on 5 CPUs each at 144 minutes (row two); the ci.yml comment
says it itself: "Add a shard when a runner is added." The change there, which
this PR does not make:

- `.github/workflows/ci.yml:225`: the self-hosted branch `'[1, 2]'` becomes
  `'[1, 2, 3, 4]'` (the hosted branch has it already).
- `.github/workflows/ci.yml:589`: `CHECK_CONCURRENCY: auto` becomes `3` for the
  shards (`auto` is 2 on 5 CPUs). Line 768, the Dependabot job's
  `task check:quick` (30-minute bound, also on `-nonprod`), stays `auto` and
  drops from 5 units to 2; not measured, watch its first run.
- `.github/workflows/ci.yml:587` and `:234`: the step bound (86) and job bound
  (96) from the replay's longest shard plus the 23-minute unit limit
  (`scripts/workflow-conditions.test.ts:591-604` fails until they hold), and
  `workflow-conditions.test.ts:571` which pins `"[1, 2]"`.

What else it costs: four shards need four PostgreSQL pairs and their `size=16g`
tmpfs, 128 GB at most against 64 GB now (a cap; host memory available at the
measurement was 218 GB of 251 GB), on top of the runners' 4 x 16 GB; a cold
`/nix` store per replica after each redeploy, four stores to warm instead of two;
and, with everything busy, CI's four runners are owed about 11.6 of 20 cores
against 13.2 now (-12%, cgroup v1 arithmetic) because the small pool's 2 x 2 and
production's doubled weight come out of it. Per-job cost goes down in one place:
a rerun of a failed shard is a quarter of a run, not half.

## Rolling this out

Dokploy autoDeploys on a merge to `main` (watch path
`dokploy/github-runner/compose.yml`) and restarts the stack: a changed replica
count, CPU limit or weight recreates the runners, and a recreated runner cuts off
the job it holds. **Merge only when no production deploy and no CI job is
running** (`gh run list -R stage-crew/tickify --status in_progress` and
`--status queued` both empty, and the Runners page shows every `dokploy-tickify-…`
runner idle), and hold CI and deploys off until Dokploy shows the stack deployed
and the four `-nonprod` runners online.

This PR's order, because the pool and tickify's shard count must change together
(row two above):

1. Set `TICKIFY_SMALL_ACCESS_TOKEN` in Dokploy.
2. Open the `stage-crew/tickify` change above (four shards, `CHECK_CONCURRENCY=3`,
   bounds) and have it approved but unmerged.
3. In one quiet window: merge this, wait for the four runners, then merge the
   tickify change. A CI run started between the two merges runs two shards on
   five-CPU runners and times out.
4. Move the small runner (above).
5. Watch the first two CI runs: the longest shard against 52-63 minutes, `reuse`
   to first shard start (queue wait), and the host's load. If the shards run past
   the bound, revert both merges, tickify's first.
