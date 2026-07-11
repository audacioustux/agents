import json
import tempfile
import unittest
from pathlib import Path

from omniroute_combo_sync import (
    OmniRouteClient,
    changed_instance_names,
    diff_combo,
    merge_existing_combo,
    normalize_model_entry,
    provider_id_from_model,
    render_markdown,
    semantic_model_entry,
    slug_model_id,
    validate_instance_config,
    validate_model_catalog,
)


class ComboSyncTests(unittest.TestCase):
    def source_config(self):
        return {
            "schemaVersion": 1,
            "instance": "omni.tux.bd",
            "baseUrl": "https://omni.tux.bd",
            "sync": {"deleteUnmanaged": False, "validateModels": True},
            "combos": [
                {
                    "name": "reasoning",
                    "title": "Reasoning",
                    "strategy": "priority",
                    "context_length": 786432,
                    "description": "Reason carefully.",
                    "tags": ["reasoning"],
                    "config": {},
                    "models": [
                        {"model": "cx/gpt-5.5-high", "providerId": "cx", "weight": 0},
                        {"model": "gh/claude-sonnet-5", "providerId": "gh", "weight": 0},
                    ],
                }
            ],
        }

    def test_provider_and_slug_helpers(self):
        self.assertEqual(provider_id_from_model("gh/claude-sonnet-5"), "gh")
        self.assertEqual(slug_model_id("cf/@cf/zai-org/glm-5.2"), "cf-cf-zai-org-glm-5-2")

    def test_normalize_model_entry_adds_kind_id_and_defaults(self):
        entry = {"model": "gh/claude-sonnet-5", "providerId": "gh", "weight": 0}
        self.assertEqual(
            normalize_model_entry("reasoning", 2, entry),
            {
                "id": "reasoning-model-2-gh-claude-sonnet-5",
                "kind": "model",
                "model": "gh/claude-sonnet-5",
                "providerId": "gh",
                "weight": 0,
            },
        )

    def test_diff_ignores_live_model_entry_ids(self):
        live = {
            "name": "reasoning",
            "strategy": "priority",
            "context_length": 786432,
            "description": "Reason carefully.",
            "config": {},
            "models": [
                {"id": "live-a", "kind": "model", "model": "cx/gpt-5.5-high", "providerId": "cx", "weight": 0},
                {"id": "live-b", "kind": "model", "model": "gh/claude-sonnet-5", "providerId": "gh", "weight": 0},
            ],
        }
        diff = diff_combo(live, self.source_config()["combos"][0])
        self.assertEqual(diff.action, "unchanged")
        self.assertEqual(diff.changed_fields, [])

    def test_semantic_model_entry_ignores_id_and_defaults(self):
        self.assertEqual(
            semantic_model_entry({"id": "anything", "model": "gh/claude-sonnet-5"}),
            {"kind": "model", "model": "gh/claude-sonnet-5", "providerId": "gh", "weight": 0},
        )

    def test_validate_config_rejects_delete_unmanaged(self):
        config = self.source_config()
        config["sync"]["deleteUnmanaged"] = True
        errors = validate_instance_config(config, Path("dokploy/omniroute/settings/instances/omni.tux.bd/combos.json"))
        self.assertIn("whole-combo deletion is not supported by this workflow", "\n".join(errors))

    def test_merge_preserves_unmanaged_live_fields(self):
        live = {
            "id": "combo-1",
            "name": "reasoning",
            "strategy": "priority",
            "context_length": 1,
            "description": "old",
            "config": {"old": True},
            "models": [],
            "sortOrder": 7,
            "isHidden": False,
            "createdAt": "old-created",
            "updatedAt": "old-updated",
            "version": 3,
        }
        merged = merge_existing_combo(live, self.source_config()["combos"][0])
        self.assertEqual(merged["id"], "combo-1")
        self.assertEqual(merged["sortOrder"], 7)
        self.assertEqual(merged["createdAt"], "old-created")
        self.assertEqual(merged["context_length"], 786432)
        self.assertEqual([m["model"] for m in merged["models"]], ["cx/gpt-5.5-high", "gh/claude-sonnet-5"])
        self.assertNotIn("tags", merged)
        self.assertNotIn("title", merged)

    def test_diff_reports_model_order_update(self):
        live = {
            "name": "reasoning",
            "strategy": "priority",
            "context_length": 786432,
            "description": "Reason carefully.",
            "config": {},
            "models": [
                {"model": "gh/claude-sonnet-5"},
                {"model": "cx/gpt-5.5-high"},
            ],
        }
        diff = diff_combo(live, self.source_config()["combos"][0])
        self.assertEqual(diff.action, "update")
        self.assertEqual(diff.changed_fields, ["models"])
        self.assertEqual(diff.after_models, ["cx/gpt-5.5-high", "gh/claude-sonnet-5"])

    def test_catalog_validation_accepts_provider_model_rows(self):
        errors = validate_model_catalog(
            self.source_config(),
            [
                {"provider": "cx", "model": "gpt-5.5-high"},
                {"provider": "gh", "model": "claude-sonnet-5"},
            ],
        )
        self.assertEqual(errors, [])

    def test_render_markdown_marks_generated_and_preserves_order(self):
        rendered = render_markdown(self.source_config())
        self.assertIn("# OmniRoute Combos — omni.tux.bd", rendered)
        self.assertIn("Generated from `combos.json`", rendered)
        self.assertLess(rendered.index("cx/gpt-5.5-high"), rendered.index("gh/claude-sonnet-5"))
        self.assertIn("Tags: `reasoning`", rendered)

    def test_changed_instances_from_workflow_input(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            self.assertEqual(changed_instance_names(repo, "workflow_dispatch", None, None, "omni.tux.bd"), ["omni.tux.bd"])

    def test_render_markdown_is_deterministic(self):
        first = render_markdown(self.source_config())
        second = render_markdown(self.source_config())
        self.assertEqual(first, second)

    def test_changed_instances_all_discovers_instance_dirs(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            instance_dir = repo / "dokploy" / "omniroute" / "settings" / "instances" / "omni.tux.bd"
            instance_dir.mkdir(parents=True)
            (instance_dir / "combos.json").write_text(json.dumps(self.source_config()), encoding="utf-8")

            self.assertEqual(changed_instance_names(repo, "workflow_dispatch", None, None, "all"), ["omni.tux.bd"])

    def test_validate_config_rejects_openai_v1_base_url(self):
        config = self.source_config()
        config["baseUrl"] = "https://omni.tux.bd/v1"

        errors = validate_instance_config(config, Path("dokploy/omniroute/settings/instances/omni.tux.bd/combos.json"))

        self.assertIn("baseUrl must be the OmniRoute origin, not the OpenAI /v1 endpoint", "\n".join(errors))

    def test_client_accepts_wrapped_combo_and_model_lists(self):
        client = object.__new__(OmniRouteClient)
        client._request = lambda method, path: {"combos": []} if path == "/api/combos" else {"models": []}

        self.assertEqual(client.get_combos(), [])
        self.assertEqual(client.get_models(), [])



if __name__ == "__main__":
    unittest.main()
