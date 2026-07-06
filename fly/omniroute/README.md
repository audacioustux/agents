# OmniRoute on Fly.io

Minimal Fly deployment using the upstream OmniRoute image.

```sh
fly apps create audacioustux-omniroute
fly volumes create omniroute_data --region sin --size 1
fly deploy
```

Import OmniRoute config after deploy; no secrets or provider configs are stored here.

## Cost

- Scales to zero: `auto_stop_machines = "suspend"`, `min_machines_running = 0`.
- No Redis by default. In-memory rate limiting is enough for a single low-cost machine.
- Add Fly Redis/Upstash later only if shared rate limits or multi-machine scaling are needed.

## Runtime

- Image: `diegosouzapw/omniroute:latest`
- Port: `20128`
- Data volume: `/app/data`
- Public API: `https://audacioustux-omniroute.fly.dev`
