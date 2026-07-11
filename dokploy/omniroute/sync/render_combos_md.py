from __future__ import annotations

import argparse
import sys
from pathlib import Path

from omniroute_combo_sync import instance_config_path, load_instance_config, render_markdown


def main() -> int:
    parser = argparse.ArgumentParser(description="Render generated OmniRoute combo Markdown.")
    parser.add_argument("--instance", required=True)
    parser.add_argument("--config-root", default="dokploy/omniroute/settings/instances")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check", action="store_true")
    args = parser.parse_args()

    try:
        config_path = instance_config_path(Path(args.config_root), args.instance)
        config = load_instance_config(config_path)
        target_path = config_path.with_name("combos.md")
        rendered = render_markdown(config)
        if args.write:
            target_path.write_text(rendered, encoding="utf-8")
            return 0
        if not target_path.exists() or target_path.read_text(encoding="utf-8") != rendered:
            print(target_path)
            return 1
        return 0
    except Exception as error:  # noqa: BLE001 - CLI boundary reports all failures.
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
