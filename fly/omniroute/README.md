# OmniRoute on Fly.io

Minimal Fly deployment for OmniRoute.

## Files

- `Dockerfile` installs the newest `omniroute` npm release that is at least 3 days old.
- `fly.toml` runs OmniRoute on port `20128`, binds `HOSTNAME=0.0.0.0`, and persists data at `/data`.

## First deploy

```sh
fly apps create audacioustux-omniroute
fly volumes create omniroute_data --region sin --size 1
fly deploy
```

Import OmniRoute config after deploy; no provider secrets are stored here.

## Redis

Default: no Redis, lowest cost. OmniRoute falls back to in-memory rate limiting.

If persistent/shared rate limits are needed, use Fly Redis/Upstash free tier and set `REDIS_URL` on the app.

## Notes

- App: `audacioustux-omniroute`
- Region: `sin`
- Data: Fly volume `omniroute_data` mounted at `/data`
- Public API: `https://audacioustux-omniroute.fly.dev`
