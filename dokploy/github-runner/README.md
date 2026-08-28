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

- Image: `myoung34/github-runner:ubuntu-noble`
- Docker: host socket mounted — workflow `services:` containers (Tickify's
  CI runs postgres:18 and redis:8) run as siblings on the host daemon.
- Persistent runner with a `/nix` volume: Tickify's workflows realize a nix
  flake per job, and the warm store turns minutes of install into seconds.
- Registration state persists in the `runner-state` volume, so redeploys
  re-attach as the same runner instead of leaking offline duplicates.

## Pointing workflows at it

In the target repo, change `runs-on: ubuntu-latest` to `runs-on: dokploy`
(the label above). Keep hosted and self-hosted interchangeable by reverting
that one line.
