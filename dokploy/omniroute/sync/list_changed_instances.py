from __future__ import annotations

import json
import os
from pathlib import Path

from omniroute_combo_sync import changed_instance_names


def main() -> int:
    instances = changed_instance_names(
        Path.cwd(),
        os.environ.get("GITHUB_EVENT_NAME", ""),
        os.environ.get("GITHUB_BASE_SHA"),
        os.environ.get("GITHUB_SHA"),
        os.environ.get("INPUT_INSTANCE"),
    )
    print(json.dumps(instances))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
