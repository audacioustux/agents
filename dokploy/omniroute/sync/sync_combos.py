from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

from omniroute_combo_sync import OmniRouteClient, apply_sync, instance_config_path, load_instance_config


def main() -> int:
    parser = argparse.ArgumentParser(description="Dry-run or apply OmniRoute combo sync.")
    parser.add_argument("--instance", required=True)
    parser.add_argument("--config-root", default="dokploy/omniroute/settings/instances")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dry-run", action="store_true")
    mode.add_argument("--apply", action="store_true")
    args = parser.parse_args()

    api_key = os.environ.get("OMNIROUTE_API_KEY")
    if not api_key:
        print("OMNIROUTE_API_KEY is required", file=sys.stderr)
        return 1

    try:
        config_path = instance_config_path(Path(args.config_root), args.instance)
        config = load_instance_config(config_path)
        client = OmniRouteClient(config["baseUrl"], api_key)
        diffs = apply_sync(client, config, dry_run=args.dry_run)
        summary = {
            "instance": args.instance,
            "dryRun": args.dry_run,
            "changes": [
                {"name": diff.name, "action": diff.action, "changedFields": diff.changed_fields}
                for diff in diffs
            ],
        }
        print(json.dumps(summary, indent=2, sort_keys=True))
        return 0
    except Exception as error:  # noqa: BLE001 - CLI boundary reports all failures.
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
