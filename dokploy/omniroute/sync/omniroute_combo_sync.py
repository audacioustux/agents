from __future__ import annotations

import copy
import json
import re
import subprocess
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


MANAGED_FIELDS = ["strategy", "context_length", "description", "config", "models"]
INSTANCE_ROOT = Path("dokploy/omniroute/settings/instances")
WORKFLOW_PATH = ".github/workflows/omniroute-combos.yml"
SYNC_PATH_PREFIX = "dokploy/omniroute/sync/"


@dataclass(frozen=True)
class ComboDiff:
    name: str
    action: str
    changed_fields: list[str]
    before_models: list[str]
    after_models: list[str]


class OmniRouteClient:
    def __init__(self, base_url: str, api_key: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key

    def get_combos(self) -> list[dict[str, Any]]:
        result = self._request("GET", "/api/combos")
        if not isinstance(result, list):
            raise RuntimeError("GET /api/combos returned non-list JSON")
        return result

    def get_models(self) -> list[dict[str, Any]]:
        result = self._request("GET", "/api/models?all=true")
        if not isinstance(result, list):
            raise RuntimeError("GET /api/models?all=true returned non-list JSON")
        return result

    def post_combo(self, payload: dict[str, Any]) -> dict[str, Any]:
        result = self._request("POST", "/api/combos", payload)
        if not isinstance(result, dict):
            raise RuntimeError("POST /api/combos returned non-object JSON")
        return result

    def put_combo(self, combo_id: str, payload: dict[str, Any]) -> dict[str, Any]:
        quoted_id = urllib.parse.quote(combo_id, safe="")
        result = self._request("PUT", f"/api/combos/{quoted_id}", payload)
        if not isinstance(result, dict):
            raise RuntimeError(f"PUT /api/combos/{combo_id} returned non-object JSON")
        return result

    def _request(self, method: str, path: str, payload: dict[str, Any] | None = None) -> Any:
        url = f"{self.base_url}{path}"
        headers = {
            "Accept": "application/json",
            "Authorization": f"Bearer {self.api_key}",
            "User-Agent": "omniroute-combo-sync/1.0",
        }
        data = None
        if payload is not None:
            data = json.dumps(payload, sort_keys=True).encode("utf-8")
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                body = response.read().decode("utf-8")
                status = response.status
        except urllib.error.HTTPError as error:
            body = error.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"{method} {url} failed with status {error.code}: {body}") from error
        except urllib.error.URLError as error:
            raise RuntimeError(f"{method} {url} failed: {error.reason}") from error
        if status < 200 or status >= 300:
            raise RuntimeError(f"{method} {url} failed with status {status}: {body}")
        try:
            return json.loads(body) if body else None
        except json.JSONDecodeError as error:
            raise RuntimeError(f"{method} {url} returned invalid JSON") from error


def instance_config_path(config_root: Path, instance: str) -> Path:
    return config_root / instance / "combos.json"


def load_instance_config(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as handle:
        data = json.load(handle)
    if not isinstance(data, dict):
        raise RuntimeError(f"{path} must contain a JSON object")
    return data


def validate_instance_config(config: dict[str, Any], path: Path) -> list[str]:
    errors: list[str] = []
    if not isinstance(config, dict):
        return ["config must be an object"]

    if config.get("schemaVersion") != 1:
        errors.append("schemaVersion must be integer 1")

    instance = config.get("instance")
    if not isinstance(instance, str) or not instance:
        errors.append("instance is required")
    else:
        path_instance = _instance_from_config_path(path)
        if path_instance is not None and path_instance != instance:
            errors.append(f"instance must match directory name {path_instance}")

    base_url = config.get("baseUrl")
    if not isinstance(base_url, str) or not base_url:
        errors.append("baseUrl is required")
    elif "/v1" in base_url.rstrip("/"):
        errors.append("baseUrl must be the OmniRoute origin, not the OpenAI /v1 endpoint")

    sync = config.get("sync", {})
    if sync is None:
        sync = {}
    if not isinstance(sync, dict):
        errors.append("sync must be an object")
        sync = {}
    if sync.get("deleteUnmanaged", False) is True:
        errors.append("whole-combo deletion is not supported by this workflow")

    combos = config.get("combos")
    if not isinstance(combos, list) or not combos:
        errors.append("combos must be a non-empty array")
        return errors

    seen_combo_names: set[str] = set()
    required_combo_fields = {"name", "title", "strategy", "context_length", "description", "tags", "config", "models"}
    required_model_fields = {"model", "providerId", "weight"}
    for combo_index, combo in enumerate(combos, start=1):
        label = f"combos[{combo_index}]"
        if not isinstance(combo, dict):
            errors.append(f"{label} must be an object")
            continue
        missing = sorted(required_combo_fields - combo.keys())
        for field in missing:
            errors.append(f"{label}.{field} is required")
        name = combo.get("name")
        if isinstance(name, str) and name:
            if name in seen_combo_names:
                errors.append(f"duplicate combo name: {name}")
            seen_combo_names.add(name)
        else:
            errors.append(f"{label}.name must be a non-empty string")
            name = f"combo-{combo_index}"
        if not isinstance(combo.get("title"), str):
            errors.append(f"{label}.title must be a string")
        if not isinstance(combo.get("strategy"), str):
            errors.append(f"{label}.strategy must be a string")
        if not isinstance(combo.get("context_length"), int):
            errors.append(f"{label}.context_length must be an integer")
        if combo.get("description") is not None and not isinstance(combo.get("description"), str):
            errors.append(f"{label}.description must be a string or null")
        tags = combo.get("tags")
        if not isinstance(tags, list) or any(not isinstance(tag, str) for tag in tags):
            errors.append(f"{label}.tags must be an array of strings")
        if not isinstance(combo.get("config"), dict):
            errors.append(f"{label}.config must be an object")
        models = combo.get("models")
        if not isinstance(models, list) or not models:
            errors.append(f"{label}.models must be a non-empty array")
            continue
        seen_models: set[str] = set()
        for model_index, model_entry in enumerate(models, start=1):
            model_label = f"{label}.models[{model_index}]"
            if not isinstance(model_entry, dict):
                errors.append(f"{model_label} must be an object")
                continue
            missing_model_fields = sorted(required_model_fields - model_entry.keys())
            for field in missing_model_fields:
                errors.append(f"{model_label}.{field} is required")
            model_id = model_entry.get("model")
            if isinstance(model_id, str) and model_id:
                if model_id in seen_models:
                    errors.append(f"duplicate model id in combo {name}: {model_id}")
                seen_models.add(model_id)
            else:
                errors.append(f"{model_label}.model must be a non-empty string")
            if not isinstance(model_entry.get("providerId"), str):
                errors.append(f"{model_label}.providerId must be a string")
            if not isinstance(model_entry.get("weight"), int):
                errors.append(f"{model_label}.weight must be an integer")
            if "kind" in model_entry and not isinstance(model_entry.get("kind"), str):
                errors.append(f"{model_label}.kind must be a string")
    return errors


def provider_id_from_model(model_id: str) -> str:
    return model_id.split("/", 1)[0]


def slug_model_id(model_id: str) -> str:
    return re.sub(r"[^A-Za-z0-9]+", "-", model_id).strip("-").lower()


def normalize_model_entry(combo_name: str, position: int, entry: dict[str, Any]) -> dict[str, Any]:
    semantic = semantic_model_entry(entry)
    return {
        "id": f"{combo_name}-model-{position}-{slug_model_id(semantic['model'])}",
        "kind": semantic["kind"],
        "model": semantic["model"],
        "providerId": semantic["providerId"],
        "weight": semantic["weight"],
    }


def semantic_model_entry(entry: dict[str, Any]) -> dict[str, Any]:
    model = entry["model"]
    return {
        "kind": entry.get("kind", "model"),
        "model": model,
        "providerId": entry.get("providerId", provider_id_from_model(model)),
        "weight": entry.get("weight", 0),
    }


def desired_combo_payload(source_combo: dict[str, Any]) -> dict[str, Any]:
    return {
        "name": source_combo["name"],
        "strategy": source_combo["strategy"],
        "context_length": source_combo["context_length"],
        "description": source_combo.get("description"),
        "config": copy.deepcopy(source_combo.get("config", {})),
        "models": [
            normalize_model_entry(source_combo["name"], position, entry)
            for position, entry in enumerate(source_combo["models"], start=1)
        ],
    }


def merge_existing_combo(live_combo: dict[str, Any], source_combo: dict[str, Any]) -> dict[str, Any]:
    merged = copy.deepcopy(live_combo)
    desired = desired_combo_payload(source_combo)
    live_ids_by_semantic: dict[tuple[Any, ...], list[str]] = {}
    for live_model in live_combo.get("models", []):
        if not isinstance(live_model, dict) or "id" not in live_model or "model" not in live_model:
            continue
        live_ids_by_semantic.setdefault(_semantic_key(semantic_model_entry(live_model)), []).append(live_model["id"])

    models = []
    for desired_model in desired["models"]:
        semantic = semantic_model_entry(desired_model)
        live_ids = live_ids_by_semantic.get(_semantic_key(semantic), [])
        model = copy.deepcopy(desired_model)
        if live_ids:
            model["id"] = live_ids.pop(0)
        models.append(model)

    for field in ("name", "strategy", "context_length", "description", "config"):
        merged[field] = desired[field]
    merged["models"] = models
    merged.pop("title", None)
    merged.pop("tags", None)
    return merged


def diff_combo(live_combo: dict[str, Any] | None, source_combo: dict[str, Any]) -> ComboDiff:
    after_models = [entry["model"] for entry in source_combo.get("models", [])]
    if live_combo is None:
        return ComboDiff(source_combo["name"], "create", MANAGED_FIELDS.copy(), [], after_models)

    before_models = [entry.get("model", "") for entry in live_combo.get("models", [])]
    changed_fields = []
    for field in ("strategy", "context_length", "description", "config"):
        if live_combo.get(field) != source_combo.get(field):
            changed_fields.append(field)
    live_models = [semantic_model_entry(entry) for entry in live_combo.get("models", [])]
    source_models = [semantic_model_entry(entry) for entry in source_combo.get("models", [])]
    if live_models != source_models:
        changed_fields.append("models")
    action = "update" if changed_fields else "unchanged"
    return ComboDiff(source_combo["name"], action, changed_fields, before_models, after_models)


def validate_model_catalog(source_config: dict[str, Any], catalog: list[dict[str, Any]]) -> list[str]:
    valid_model_ids: set[str] = set()
    for row in catalog:
        if not isinstance(row, dict):
            continue
        row_id = row.get("id")
        if isinstance(row_id, str):
            valid_model_ids.add(row_id)
        provider = row.get("provider")
        model = row.get("model")
        if isinstance(provider, str) and isinstance(model, str):
            valid_model_ids.add(f"{provider}/{model}")

    errors: list[str] = []
    for combo in source_config.get("combos", []):
        combo_name = combo.get("name", "<unknown>")
        for entry in combo.get("models", []):
            model_id = entry.get("model")
            if model_id not in valid_model_ids:
                errors.append(f"{combo_name}: unknown model id {model_id}")
    return errors


def plan_sync(source_config: dict[str, Any], live_combos: list[dict[str, Any]]) -> list[ComboDiff]:
    live_by_name = {combo.get("name"): combo for combo in live_combos if isinstance(combo, dict)}
    return [diff_combo(live_by_name.get(combo["name"]), combo) for combo in source_config.get("combos", [])]


def apply_sync(client: OmniRouteClient, source_config: dict[str, Any], *, dry_run: bool) -> list[ComboDiff]:
    config_path = Path("dokploy/omniroute/settings/instances") / str(source_config.get("instance", "")) / "combos.json"
    config_errors = validate_instance_config(source_config, config_path)
    if config_errors:
        raise RuntimeError("invalid combo config:\n" + "\n".join(config_errors))

    live_combos = client.get_combos()
    if source_config.get("sync", {}).get("validateModels", True):
        catalog_errors = validate_model_catalog(source_config, client.get_models())
        if catalog_errors:
            raise RuntimeError("invalid combo models:\n" + "\n".join(catalog_errors))

    diffs = plan_sync(source_config, live_combos)
    if dry_run:
        return diffs

    live_by_name = {combo.get("name"): combo for combo in live_combos if isinstance(combo, dict)}
    source_by_name = {combo["name"]: combo for combo in source_config.get("combos", [])}
    for diff in diffs:
        if diff.action == "unchanged":
            continue
        source_combo = source_by_name[diff.name]
        if diff.action == "create":
            client.post_combo(desired_combo_payload(source_combo))
        elif diff.action == "update":
            live_combo = live_by_name[diff.name]
            combo_id = live_combo.get("id")
            if not isinstance(combo_id, str) or not combo_id:
                raise RuntimeError(f"{diff.name}: live combo is missing id for PUT")
            client.put_combo(combo_id, merge_existing_combo(live_combo, source_combo))

    verified_combos = client.get_combos()
    verification_diffs = plan_sync(source_config, verified_combos)
    failed = [diff for diff in verification_diffs if diff.action != "unchanged"]
    if failed:
        details = "; ".join(f"{diff.name}: {', '.join(diff.changed_fields)}" for diff in failed)
        raise RuntimeError(f"post-apply verification failed: {details}")
    return diffs


def render_markdown(source_config: dict[str, Any]) -> str:
    lines = [
        f"# OmniRoute Combos — {source_config['instance']}",
        "",
        "> Generated from `combos.json`. Edit the JSON source, not this file.",
    ]
    for combo in source_config.get("combos", []):
        tags = combo.get("tags", [])
        tag_text = "_none_" if not tags else f"`{', '.join(tags)}`"
        description = combo.get("description")
        description_text = "_none_" if description is None else str(description)
        lines.extend(
            [
                "",
                f"## {combo['name']}",
                "",
                f"- Strategy: `{combo['strategy']}`",
                f"- Context length: `{combo['context_length']}`",
                f"- Tags: {tag_text}",
                f"- Description: {description_text}",
                "",
                "| # | Model | Provider | Weight |",
                "|---:|---|---|---:|",
            ]
        )
        for index, entry in enumerate(combo.get("models", []), start=1):
            semantic = semantic_model_entry(entry)
            lines.append(
                f"| {index} | `{semantic['model']}` | `{semantic['providerId']}` | {semantic['weight']} |"
            )
    return "\n".join(lines) + "\n"


def changed_instance_names(
    repo_root: Path,
    event_name: str,
    base_ref: str | None,
    head_ref: str | None,
    workflow_input: str | None,
) -> list[str]:
    instance_input = (workflow_input or "").strip()
    if event_name == "workflow_dispatch":
        if instance_input and instance_input != "all":
            return [instance_input]
        return _known_instance_names(repo_root)

    if event_name == "push":
        if not base_ref or not head_ref:
            return []
        completed = subprocess.run(
            ["git", "diff", "--name-only", base_ref, head_ref],
            cwd=repo_root,
            check=True,
            capture_output=True,
            text=True,
        )
        changed_paths = [line.strip() for line in completed.stdout.splitlines() if line.strip()]
        if any(path == WORKFLOW_PATH or path.startswith(SYNC_PATH_PREFIX) for path in changed_paths):
            return _known_instance_names(repo_root)
        prefix = INSTANCE_ROOT.as_posix() + "/"
        instances = set()
        for changed_path in changed_paths:
            if not changed_path.startswith(prefix) or not changed_path.endswith("/combos.json"):
                continue
            relative = changed_path.removeprefix(prefix)
            parts = relative.split("/")
            if len(parts) == 2 and parts[1] == "combos.json":
                instances.add(parts[0])
        return sorted(instances)

    return _known_instance_names(repo_root)


def _instance_from_config_path(path: Path) -> str | None:
    parts = path.as_posix().split("/")
    for index, part in enumerate(parts):
        if part == "instances" and index + 2 < len(parts) and parts[index + 2] == "combos.json":
            return parts[index + 1]
    return None


def _known_instance_names(repo_root: Path) -> list[str]:
    instances_root = repo_root / INSTANCE_ROOT
    if not instances_root.exists():
        return []
    names = [path.parent.name for path in instances_root.glob("*/combos.json") if path.is_file()]
    return sorted(names)


def _semantic_key(entry: dict[str, Any]) -> tuple[Any, ...]:
    return (entry["kind"], entry["model"], entry["providerId"], entry["weight"])
