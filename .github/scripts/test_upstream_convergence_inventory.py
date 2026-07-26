import json
import unittest
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

    def test_external_agent_preflight_proof_is_intentionally_owned(self) -> None:
        classified = inventory.classify(
            "codex-rs/core/tests/suite/external_agent_preflight.rs",
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
        self.assertEqual(
            classified["contracts"], ["AGENT-1", "INTEGRATION-1", "VALIDATION-1"]
        )

    def test_classify_path_omits_conflict_type(self) -> None:
        self.assertNotIn(
            "conflictType", inventory.classify_path("codex-rs/core/src/lib.rs")
        )


class OwnedFeatureCoverageTest(unittest.TestCase):
    """Owned implementations and the proofs that pin them classify together.

    The anchor merge deleted owned code and owned coverage in one silent step, so
    a feature whose implementation is guarded while its integration proof is not
    can still lose its evidence without any path going missing.
    """

    def assert_owned(self, path: str, contract: str) -> None:
        classified = inventory.classify_path(path)
        self.assertEqual("intentionally_owned", classified["lane"], path)
        self.assertIn(contract, classified["contracts"], path)

    def test_project_validation_implementation_and_proofs_are_owned(self) -> None:
        for path in (
            "codex-rs/core/src/session/project_validation.rs",
            "codex-rs/core/src/session/project_validation_coordinator.rs",
            "codex-rs/core/src/session/validation_provider.rs",
            "codex-rs/core/src/session/cargo_validation_provider.rs",
            "codex-rs/core/src/context/project_validation_failure.rs",
            "codex-rs/tui/src/history_cell/project_validation.rs",
            "codex-rs/core/tests/suite/project_validation.rs",
            "codex-rs/exec/tests/suite/project_validation_event.rs",
            "codex-rs/app-server-protocol/src/protocol/v2/validation.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "VALIDATION-1")

    def test_background_review_implementation_and_proof_are_owned(self) -> None:
        for path in (
            "codex-rs/core/src/session/background_auto_review.rs",
            "codex-rs/core/src/session/background_auto_review_tests.rs",
            "codex-rs/core/tests/suite/background_review.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "AGENT-1")

    def test_background_review_engine_and_session_state_are_owned(self) -> None:
        # The engine that runs a background review and the durable per-session
        # state it reads back. Upstream owns both filenames, so only these exact
        # paths are claimed.
        for path in (
            "codex-rs/core/src/tasks/review.rs",
            "codex-rs/core/src/tasks/review_tests.rs",
            "codex-rs/core/src/state/session.rs",
            "codex-rs/core/src/state/session_tests.rs",
            "codex-rs/tui/src/agent_session_env.rs",
            "codex-rs/tui/src/agent_session_env_tests.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "AGENT-1")

    def test_owned_review_wire_fixtures_are_owned(self) -> None:
        # Additive Every Code schemas with no upstream counterpart, guarded the
        # same way the Project Validation fixtures are.
        for path in (
            "codex-rs/app-server-protocol/schema/json/v2/BackgroundAutoReviewControlParams.json",
            "codex-rs/app-server-protocol/schema/typescript/v2/AutoReviewRunSummary.ts",
            "codex-rs/app-server-protocol/schema/typescript/v2/SessionProvenance.ts",
            "codex-rs/app-server-protocol/schema/typescript/v2/ReviewStartTarget.ts",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "AGENT-1")

    def test_approval_compatibility_shims_are_owned(self) -> None:
        # Older clients still send the retired approval vocabulary on every
        # external entry point; dropping a shim rejects requests that used to work.
        for path in (
            "codex-rs/mcp-server/src/approval_response_compat_tests.rs",
            "codex-rs/protocol/src/review_decision_compat.rs",
            "codex-rs/protocol/src/review_decision_compat_tests.rs",
            "codex-rs/utils/cli/src/approval_mode_cli_arg.rs",
            "codex-rs/utils/cli/src/approval_mode_cli_arg_tests.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "SANDBOX-1")

    def test_history_rewrite_exception_proof_is_owned(self) -> None:
        self.assert_owned(
            "codex-rs/core/tests/suite/invalid_image_recovery.rs", "CONTEXT-1"
        )

    def test_code_bridge_and_browser_handlers_and_proofs_are_owned(self) -> None:
        for path in (
            "codex-rs/core/src/tools/handlers/code_bridge.rs",
            "codex-rs/core/src/tools/handlers/code_bridge_tests.rs",
            "codex-rs/core/src/tools/handlers/browser.rs",
            "codex-rs/core/src/tools/handlers/browser_spec_tests.rs",
            "codex-rs/core/src/browser.rs",
            "codex-rs/core/src/browser_tests.rs",
            "codex-rs/app-server/tests/suite/v2/code_bridge.rs",
            "codex-rs/app-server/tests/suite/v2/remote_control.rs",
            "codex-rs/app-server/src/request_processors/remote_control_processor.rs",
            # The three named model-facing Code Bridge proofs live here.
            "codex-rs/core/tests/suite/tools.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "INTEGRATION-1")

    def test_external_agent_preflight_and_routing_proofs_are_owned(self) -> None:
        for path in (
            "codex-rs/core/tests/suite/external_agent_preflight.rs",
            "codex-rs/core/src/agent/external_preflight.rs",
            "codex-rs/core/src/agent/external_preflight_tests.rs",
            "codex-rs/core/src/agent/provider_routing.rs",
            "codex-rs/core/src/agent/provider_routing_tests.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "AGENT-1")

    def test_proof_registries_are_owned_so_suites_cannot_be_unregistered(self) -> None:
        # Reverting a suite registry leaves every owned proof file in the tree
        # while silently removing it from the test binary.
        for path in inventory.SHARED_PROOF_REGISTRIES:
            with self.subTest(path=path):
                classified = inventory.classify_path(path)
                self.assertEqual("intentionally_owned", classified["lane"])
                # A registry may also carry a surface contract of its own crate,
                # so require the owned proof contracts rather than an exact set.
                self.assertLessEqual(
                    {"AGENT-1", "INTEGRATION-1", "VALIDATION-1"},
                    set(classified["contracts"]),
                )

    def test_owned_app_server_v2_proofs_are_owned(self) -> None:
        for path, contract in (
            (
                "codex-rs/app-server/tests/suite/v2/background_review_control.rs",
                "AGENT-1",
            ),
            ("codex-rs/app-server/tests/suite/v2/project_validation.rs", "VALIDATION-1"),
        ):
            with self.subTest(path=path):
                self.assert_owned(path, contract)

    def test_durable_environment_baseline_writer_reader_and_proofs_are_owned(
        self,
    ) -> None:
        for path in (
            "codex-rs/core/src/session/turn_context.rs",
            "codex-rs/core/src/session/rollout_reconstruction.rs",
            "codex-rs/core/src/session/rollout_reconstruction_tests.rs",
            "codex-rs/core/src/context/world_state/environment.rs",
            "codex-rs/core/src/context/world_state/environment_limits.rs",
            "codex-rs/core/src/context/world_state/mod.rs",
            "codex-rs/core/tests/suite/turn_context_environments.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "HISTORY-1")

    def test_model_visible_context_safety_paths_are_owned(self) -> None:
        for path in (
            "codex-rs/core/src/context/token_budget_context.rs",
            "codex-rs/core/src/context/token_budget_context_tests.rs",
            "codex-rs/core/src/context_manager/history.rs",
            "codex-rs/core/src/context_manager/history_tests.rs",
            "codex-rs/core/src/session_prefix.rs",
            "codex-rs/core/src/session_prefix_tests.rs",
            "codex-rs/core/src/session/turn.rs",
            "codex-rs/core/tests/suite/view_image.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "CONTEXT-1")

    def test_hook_identity_paths_are_owned(self) -> None:
        for path in (
            "codex-rs/config/src/hook_config.rs",
            "codex-rs/config/src/hooks_tests.rs",
            "codex-rs/hooks/src/declarations.rs",
            "codex-rs/hooks/src/engine/discovery.rs",
            "codex-rs/hooks/src/engine/mod_tests.rs",
            "codex-rs/hooks/src/lib.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "HOOKS-1")

    def test_background_review_replay_and_provenance_paths_are_owned(self) -> None:
        for path in (
            "codex-rs/tui/src/app/thread_routing.rs",
            "codex-rs/tui/src/app/test_support.rs",
            "codex-rs/core/tests/suite/session_provenance.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "AGENT-1")

    def test_shared_cli_option_surface_and_proof_are_owned(self) -> None:
        for path in (
            "codex-rs/utils/cli/src/shared_options.rs",
            "codex-rs/exec/tests/suite/shared_cli_options.rs",
        ):
            with self.subTest(path=path):
                self.assert_owned(path, "AUTH-1")

    def test_feature_stems_do_not_claim_unrelated_upstream_modules(self) -> None:
        # `validation` is a word upstream uses for config, cloud, OTEL, and
        # request validation. Only the Project Validation stems are owned. The
        # same applies to the newer stems: the shared-module rules name exact
        # files so a neighbouring upstream module in the same directory stays
        # green.
        for path in (
            "codex-rs/config/src/validation.rs",
            "codex-rs/cloud-config/src/validation.rs",
            "codex-rs/otel/src/metrics/validation.rs",
            "codex-rs/tui/src/chatwidget/tests/goal_validation.rs",
            "codex-rs/core/src/tools/handlers/shell.rs",
            "codex-rs/core/src/session/token_budget.rs",
            "codex-rs/core/src/context_manager/normalize.rs",
            "codex-rs/hooks/src/registry.rs",
            "codex-rs/utils/cli/src/resume_command.rs",
            "codex-rs/tui/src/app/thread_events.rs",
        ):
            with self.subTest(path=path):
                self.assertEqual(
                    "green_bulk_adopt", inventory.classify_path(path)["lane"]
                )

    def test_feature_paths_covers_implementation_and_proof_roots(self) -> None:
        patterns = inventory.feature_paths("project_validation")

        self.assertIn("codex-rs/core/src/session/project_validation*", patterns)
        self.assertIn("codex-rs/core/tests/suite/project_validation*", patterns)
        self.assertEqual(
            len(inventory.IMPLEMENTATION_ROOTS) + len(inventory.PROOF_ROOTS),
            len(patterns),
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

    def test_no_snapshot_claims_residual_paths_were_rejected(self) -> None:
        for snapshot in self.snapshots():
            with self.subTest(snapshot=snapshot.name):
                markdown = (snapshot / "inventory.md").read_text(encoding="utf-8")
                self.assertNotIn("rejected", markdown)


if __name__ == "__main__":
    unittest.main()
