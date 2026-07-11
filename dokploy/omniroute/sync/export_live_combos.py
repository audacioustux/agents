from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path
from typing import Any

from omniroute_combo_sync import OmniRouteClient, semantic_model_entry


def main() -> int:
    parser = argparse.ArgumentParser(description="Export live OmniRoute combos to structured JSON.")
    parser.add_argument("--instance", required=True)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--tags-from", default="dokploy/omniroute/settings/combos.md")
    args = parser.parse_args()

    api_key = os.environ.get("OMNIROUTE_API_KEY")
    if not api_key:
        print("OMNIROUTE_API_KEY is required", file=sys.stderr)
        return 1

    try:
        client = OmniRouteClient(args.base_url, api_key)
        live_combos = client.get_combos()
        tags_by_combo = _parse_tags(Path(args.tags_from))
        exported = {
            "schemaVersion": 1,
            "instance": args.instance,
            "baseUrl": args.base_url.rstrip("/"),
            "sync": {"deleteUnmanaged": False, "validateModels": True},
            "combos": [_export_combo(combo, tags_by_combo) for combo in sorted(live_combos, key=_combo_sort_key)],
        }
        output_path = Path(args.out)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(json.dumps(exported, indent=2, sort_keys=False) + "\n", encoding="utf-8")
        return 0
    except Exception as error:  # noqa: BLE001 - CLI boundary reports all failures.
        print(str(error), file=sys.stderr)
        return 1


def _combo_sort_key(combo: dict[str, Any]) -> tuple[int, str]:
    sort_order = combo.get("sortOrder")
    if not isinstance(sort_order, int):
        sort_order = 1_000_000
    name = combo.get("name")
    return sort_order, name if isinstance(name, str) else ""


def _export_combo(combo: dict[str, Any], tags_by_combo: dict[str, list[str]]) -> dict[str, Any]:
    name = combo["name"]
    return {
        "name": name,
        "title": _title_from_name(name),
        "strategy": combo.get("strategy", "priority"),
        "context_length": combo.get("context_length"),
        "description": combo.get("description"),
        "tags": tags_by_combo.get(name, []),
        "config": combo.get("config", {}),
        "models": [_export_model_entry(entry) for entry in combo.get("models", [])],
    }


def _export_model_entry(entry: dict[str, Any]) -> dict[str, Any]:
    semantic = semantic_model_entry(entry)
    exported = {
        "model": semantic["model"],
        "providerId": semantic["providerId"],
        "weight": semantic["weight"],
    }
    if semantic["kind"] != "model":
        exported["kind"] = semantic["kind"]
    return exported


def _title_from_name(name: str) -> str:
    return " ".join(part.capitalize() for part in name.split("-"))


def _parse_tags(path: Path) -> dict[str, list[str]]:
    if not path.exists():
        return {}
    tags_by_combo: dict[str, list[str]] = {}
    current_combo: str | None = None
    heading_re = re.compile(r"^##\s+([^\s(]+)")
    tag_re = re.compile(r"`([^`]+)`")
    for line in path.read_text(encoding="utf-8").splitlines():
        heading = heading_re.match(line)
        if heading:
            current_combo = heading.group(1)
            continue
        if current_combo and line.startswith("- Tags:"):
            tags_by_combo[current_combo] = tag_re.findall(line)
    return tags_by_combo


if __name__ == "__main__":
    raise SystemExit(main())
