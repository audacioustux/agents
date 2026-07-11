from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class ComboDiff:
    name: str
    action: str
    changed_fields: list[str]
    before_models: list[str]
    after_models: list[str]


class OmniRouteClient:
    def __init__(self, base_url: str, api_key: str) -> None:
        raise NotImplementedError

    def get_combos(self) -> list[dict[str, Any]]:
        raise NotImplementedError

    def get_models(self) -> list[dict[str, Any]]:
        raise NotImplementedError

    def post_combo(self, payload: dict[str, Any]) -> dict[str, Any]:
        raise NotImplementedError

    def put_combo(self, combo_id: str, payload: dict[str, Any]) -> dict[str, Any]:
        raise NotImplementedError


def instance_config_path(config_root: Path, instance: str) -> Path:
    raise NotImplementedError


def load_instance_config(path: Path) -> dict[str, Any]:
    raise NotImplementedError


def validate_instance_config(config: dict[str, Any], path: Path) -> list[str]:
    raise NotImplementedError


def provider_id_from_model(model_id: str) -> str:
    raise NotImplementedError


def slug_model_id(model_id: str) -> str:
    raise NotImplementedError


def normalize_model_entry(combo_name: str, position: int, entry: dict[str, Any]) -> dict[str, Any]:
    raise NotImplementedError


def semantic_model_entry(entry: dict[str, Any]) -> dict[str, Any]:
    raise NotImplementedError


def desired_combo_payload(source_combo: dict[str, Any]) -> dict[str, Any]:
    raise NotImplementedError


def merge_existing_combo(live_combo: dict[str, Any], source_combo: dict[str, Any]) -> dict[str, Any]:
    raise NotImplementedError


def diff_combo(live_combo: dict[str, Any] | None, source_combo: dict[str, Any]) -> ComboDiff:
    raise NotImplementedError


def validate_model_catalog(source_config: dict[str, Any], catalog: list[dict[str, Any]]) -> list[str]:
    raise NotImplementedError


def plan_sync(source_config: dict[str, Any], live_combos: list[dict[str, Any]]) -> list[ComboDiff]:
    raise NotImplementedError


def apply_sync(client: OmniRouteClient, source_config: dict[str, Any], *, dry_run: bool) -> list[ComboDiff]:
    raise NotImplementedError


def render_markdown(source_config: dict[str, Any]) -> str:
    raise NotImplementedError


def changed_instance_names(
    repo_root: Path,
    event_name: str,
    base_ref: str | None,
    head_ref: str | None,
    workflow_input: str | None,
) -> list[str]:
    raise NotImplementedError
