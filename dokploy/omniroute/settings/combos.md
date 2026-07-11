# OmniRoute Combo Settings

Combo configuration is now structured per OmniRoute instance.

- `instances/omni.tux.bd/combos.json` — source of truth for the `omni.tux.bd` instance.
- `instances/omni.tux.bd/combos.md` — generated human-readable view; do not edit by hand.

To change live combo models, edit the instance `combos.json` file and merge to `main`. The GitHub Actions workflow applies declared combo changes with the `omni.tux.bd` GitHub Environment secret.
