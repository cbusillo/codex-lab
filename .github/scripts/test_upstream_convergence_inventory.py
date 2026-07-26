import json
import unittest
from collections import Counter
from pathlib import Path

import upstream_convergence_inventory as inventory


SNAPSHOT_ROOT = Path(__file__).resolve().parents[2] / "upstream" / "openai-codex"


class UpstreamConvergenceInventoryTest(unittest.TestCase):
    def test_parses_content_conflict(self) -> None:
        self.assertEqual(
            inventory.parse_conflict_message(
                "CONFLICT (content): Merge conflict in codex-rs/core/src/lib.rs"
            ),
            ("content", "codex-rs/core/src/lib.rs"),
        )

    def test_parses_modify_delete_conflict(self) -> None:
        self.assertEqual(
            inventory.parse_conflict_message(
                "CONFLICT (modify/delete): path/file.rs deleted in local and modified in upstream. Version upstream of path/file.rs left in tree."
            ),
            ("modify/delete", "path/file.rs"),
        )

    def test_defaults_unowned_path_to_upstream(self) -> None:
        self.assertEqual(
            inventory.classify("codex-rs/core/src/lib.rs", "content"),
            {
                "path": "codex-rs/core/src/lib.rs",
                "conflictType": "content",
                "lane": "green_bulk_adopt",
                "contracts": [],
                "reason": "upstream-owned surface with no named local contract",
            },
        )

    def test_red_lane_wins_when_contracts_overlap(self) -> None:
        classified = inventory.classify("codex-rs/cli/src/login.rs", "content")
        self.assertEqual(classified["lane"], "red_manual_review")
        self.assertEqual(classified["contracts"], ["IDENTITY-1"])

    def test_protocol_path_is_contract_adapted(self) -> None:
        classified = inventory.classify(
            "codex-rs/app-server-protocol/src/protocol/v2/account.rs",
            "content",
        )
        self.assertEqual(classified["lane"], "amber_contract_adapt")
        self.assertEqual(classified["contracts"], ["PROTOCOL-1"])

    def test_agent_path_is_intentionally_owned(self) -> None:
        classified = inventory.classify(
            "codex-rs/core/src/agent/control.rs",
            "content",
        )
        self.assertEqual(classified["lane"], "intentionally_owned")
        self.assertEqual(classified["contracts"], ["AGENT-1"])

    def test_proof_harness_is_intentionally_owned(self) -> None:
        classified = inventory.classify(
            "tools/codex-exec-harness/scenarios/background-review-same-turn-commit.json",
            "content",
        )
        self.assertEqual(classified["lane"], "intentionally_owned")
        self.assertEqual(classified["contracts"], ["AGENT-1", "INTEGRATION-1"])

    def test_tui_provenance_diagnostics_require_manual_review(self) -> None:
        self.assertEqual(
            inventory.classify_path("codex-rs/tui/src/debug_config.rs"),
            {
                "path": "codex-rs/tui/src/debug_config.rs",
                "lane": "red_manual_review",
                "contracts": ["RELEASE-1"],
                "reason": "local build provenance diagnostics",
            },
        )

    def test_dogfood_launcher_is_intentionally_owned(self) -> None:
        self.assertEqual(
            inventory.classify_path("scripts/local/install-codex-lab-dev.sh"),
            {
                "path": "scripts/local/install-codex-lab-dev.sh",
                "lane": "intentionally_owned",
                "contracts": ["RELEASE-1"],
                "reason": "Every Code distribution authority",
            },
        )

    def test_classify_path_omits_conflict_type(self) -> None:
        self.assertNotIn(
            "conflictType", inventory.classify_path("codex-rs/core/src/lib.rs")
        )


class ResidualSemanticsTest(unittest.TestCase):
    """Residual paths are *retained* by the merge, not rejected by it."""

    def sample_inventory(self) -> dict[str, object]:
        residuals = [inventory.classify_path("codex-rs/auto-review/src/lib.rs")]
        return {
            "schemaVersion": inventory.SCHEMA_VERSION,
            "repository": "openai/codex",
            "refs": {"base": "a" * 40, "upstream": "b" * 40, "local": "c" * 40},
            "policy": {"defaultLane": "green_bulk_adopt", "rule": "Upstream wins."},
            "summary": {
                "conflicts": 0,
                "localChangedOnly": 1,
                "sharedIdentical": 0,
                "sharedMergeableDivergent": 0,
                "residualLocalInfluence": len(residuals),
            },
            "conflictTypeCounts": {},
            "laneCounts": {},
            "residualLaneCounts": {"intentionally_owned": 1},
            "conflicts": [],
            "residuals": residuals,
        }

    def test_markdown_describes_residuals_as_retained(self) -> None:
        markdown = inventory.render_markdown(self.sample_inventory())

        self.assertIn(
            "Residual local-influence paths retained by an upstream-first merge: 1",
            markdown,
        )
        self.assertNotIn("rejected", markdown)

    def test_markdown_reports_residual_lane_counts(self) -> None:
        markdown = inventory.render_markdown(self.sample_inventory())

        self.assertIn("| Residual lane `intentionally_owned` | 1 |", markdown)

    def test_residual_output_is_machine_readable(self) -> None:
        document = json.loads(inventory.render_residuals(self.sample_inventory()))

        self.assertEqual(inventory.SCHEMA_VERSION, document["schemaVersion"])
        self.assertEqual(1, document["summary"]["residualLocalInfluence"])
        self.assertEqual(
            [
                {
                    "path": "codex-rs/auto-review/src/lib.rs",
                    "lane": "intentionally_owned",
                    "contracts": ["AGENT-1"],
                    "reason": "Every Code orchestration and review behavior",
                }
            ],
            document["residuals"],
        )

    def test_inventory_json_omits_the_residual_list(self) -> None:
        document = json.loads(inventory.render_json(self.sample_inventory()))

        self.assertNotIn("residuals", document)
        self.assertEqual(1, document["summary"]["residualLocalInfluence"])


class CheckedInSnapshotTest(unittest.TestCase):
    def snapshots(self) -> list[Path]:
        return sorted(path for path in SNAPSHOT_ROOT.iterdir() if path.is_dir())

    def test_every_snapshot_records_current_schema_and_residuals(self) -> None:
        self.assertTrue(self.snapshots())
        for snapshot in self.snapshots():
            with self.subTest(snapshot=snapshot.name):
                summary = json.loads(
                    (snapshot / "inventory.json").read_text(encoding="utf-8")
                )
                residuals = json.loads(
                    (snapshot / "residuals.json").read_text(encoding="utf-8")
                )
                self.assertEqual(inventory.SCHEMA_VERSION, summary["schemaVersion"])
                self.assertNotIn("silentLocalInfluence", summary["summary"])
                self.assertEqual(
                    summary["summary"]["residualLocalInfluence"],
                    len(residuals["residuals"]),
                )
                residual_lane_counts = dict(
                    sorted(Counter(item["lane"] for item in residuals["residuals"]).items())
                )
                self.assertEqual(
                    residual_lane_counts,
                    summary["residualLaneCounts"],
                )
                self.assertEqual(
                    residual_lane_counts,
                    residuals["residualLaneCounts"],
                )
                markdown = (snapshot / "inventory.md").read_text(encoding="utf-8")
                for lane, count in residual_lane_counts.items():
                    self.assertIn(f"| Residual lane `{lane}` | {count} |", markdown)

    def test_no_snapshot_claims_residual_paths_were_rejected(self) -> None:
        for snapshot in self.snapshots():
            with self.subTest(snapshot=snapshot.name):
                markdown = (snapshot / "inventory.md").read_text(encoding="utf-8")
                self.assertNotIn("rejected", markdown)


if __name__ == "__main__":
    unittest.main()
