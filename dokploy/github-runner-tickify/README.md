# Tickify's production runner

The runner for stage-crew/tickify's production jobs, the ones that bind its
`tickify / production` GitHub environment: label `dokploy-tickify`, runners
`dokploy-tickify-<n>`, one job per container, no Docker socket, no host
network, jobs as a non-root user. Every other tickify job runs on
`dokploy-tickify-nonprod` (`../github-runner`). It shares
`../github-runner/register-entrypoint.sh` and its credential rules
(`../github-runner/README.md`, "Registration credential"), but it is not a
Dokploy service: isolation is a property of the host as much as of this file.

## Host

A VM that holds nothing else and that the CI pool's host cannot reach. On one
Docker daemon the CI pool's mounted socket reaches this container, so the
settings in `compose.yml` would protect nothing. That rules out:

- **The CI pool's host.** Any CI job there owns that daemon.
- **A host the CI pool's Dokploy manages.** Dokploy holds SSH access to its
  remote servers, and a CI job with the Docker socket reads Dokploy's own state
  on the host it runs on. This VM is plain Docker and systemd.
- **A host that stores another project's secrets**, or whose network reaches
  the CI pool's.

## Deploy

In the order `../github-runner/README.md`, "Pointing workflows at a pool",
gives:

1. Clone this repository to `/opt/agents` on the VM.
2. Create a fine-grained token for this runner alone: resource owner
   `stage-crew`, repository `tickify` only, `Administration: write` only.
   Type or paste it into `/etc/github-runner-tickify/access-token` on the VM,
   owner root, mode 0400; never into a variable or a shell history.
3. Copy `.env.example` to `.env` here and fill the image digest.
4. Install `github-runner-tickify.service` into `/etc/systemd/system` and
   `systemctl enable --now github-runner-tickify`. The runner shows in the
   repository's runners list as `dokploy-tickify-…` with the one label.
5. Deploy `../github-runner`'s compose from this commit: it drops that host's
   production runner and the CI pool's `dokploy-tickify`.
6. Canary: dispatch tickify's `check-ci-role.yml` (read-only), then
   `deploy-tickify-production.yml` with `plan_only`, and read each job log's
   runner name: it must be `dokploy-tickify-…`, not `…-nonprod-…`. The plan
   run is also the measurement of this runner's CPU and memory limits.

To change the image or this directory: edit `.env`, `docker compose pull`,
`systemctl restart github-runner-tickify` (that stops a running job's
container; do it between deploys).

## Not closed

- **A forged runner.** Anyone holding a registration credential for the
  repository chooses its runner's labels. The CI pool's jobs can still read
  that pool's access token (root, Docker socket), register a runner labelled
  `dokploy-tickify`, and be handed production jobs. Only a CI pool without the
  socket and with non-root jobs closes that.
- **The registration token** (one hour) is in PID 1's environment on both
  pools; on the CI pool a job can read it within that hour of a start.
- **Untested here:** a container (no Docker daemon where this was written),
  that Compose keeps the host file's owner and mode on a file secret (the
  wrapper refuses to start if it does not), `docker compose run` forwarding
  SIGTERM, and production's jobs under `no-new-privileges` (none of them
  calls `sudo` or `apt`, read 2026-10-18).
- **One job at a time.** Production's jobs queue behind each other; a second
  instance of the unit is the remedy if that is ever too slow (unmeasured).
- **One VM.** With it down, nothing deploys production: tickify's
  `docs/operations/deployment.md` has the break-glass.
