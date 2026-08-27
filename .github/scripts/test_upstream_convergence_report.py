import json
import tempfile
import unittest
from datetime import datetime
from datetime import timedelta
from datetime import timezone
from pathlib import Path
from unittest.mock import call
from unittest.mock import patch

import upstream_convergence_report as report


NOW = datetime(2026, 8, 22, 18, 0, tzinfo=timezone.utc)


def inspection_report() -> dict[str, object]:
    return {
        "refs": {"base": "base-sha", "upstream": "upstream-sha", "local": "local-sha"},
        "inventory": {
            "summary": {"conflicts": 2, "residualLocalInfluence": 7},
            "laneCounts": {"red_manual_review": 1, "green_bulk_adopt": 4},
        },
    }


def telemetry_env() -> dict[str, str]:
    return {
        "GITHUB_RUN_ID": "123456",
        "GITHUB_RUN_ATTEMPT": "2",
        "GITHUB_EVENT_NAME": "schedule",
        "GITHUB_WORKFLOW": "Upstream Convergence Inspection",
        "GITHUB_SHA": "local-sha",
        "GITHUB_REF_NAME": "main",
        "INSPECTION_STARTED_AT": "2026-08-22T17:59:59Z",
        "INSPECTION_FINISHED_AT": "2026-08-22T18:00:00Z",
        "INSPECTION_DURATION_MS": "1500",
    }


def candidate_gate(
    *, due: bool = False, reasons: list[str] | None = None
) -> dict[str, object]:
    return {
        "status": "ready",
        "due": due,
        "thresholds": {"maxAgeHours": 72, "maxUpstreamCommits": 75},
        "latestIntegratedSnapshot": {
            "snapshot": "upstream/openai-codex/b89ce9a2-4462b9de",
            "refs": {
                "base": "b89ce9a2bcedcfddf3a48f387b7912d602d6d87c",
                "upstream": "4462b9deef211723b781b426f5e5d36a5777115f",
                "local": "8add494682f7c0674672e8dc5b38a4565cd7629b",
            },
            "recordedAt": "2026-08-19T16:02:00Z",
            "ageHours": 73.0 if due else 0.0,
            "upstreamCommits": 0,
        },
        "reasons": reasons or [],
    }


def cycle_telemetry() -> dict[str, object]:
    return {
        "run": {
            "workflow": "Upstream Convergence Inspection",
            "runId": "123456",
            "runAttempt": 2,
            "eventName": "schedule",
            "sha": "local-sha",
            "refName": "main",
            "cycleId": "123456:2:schedule",
        },
        "inspection": {
            "startedAt": "2026-08-22T17:59:59Z",
            "finishedAt": "2026-08-22T18:00:00Z",
            "reportGeneratedAt": "2026-08-22T18:00:00Z",
            "durationMs": 1500,
        },
        "modelTelemetry": {
            "calls": 0,
            "promptTokens": 0,
            "completionTokens": 0,
            "totalTokens": 0,
            "accountingConfidence": "explicit_zero",
            "modelFree": True,
        },
    }


class TestUpstreamConvergenceReport(unittest.TestCase):
    @patch("upstream_convergence_report.run_git")
    def test_snapshot_timestamp_uses_first_parent_integration_time(
        self, run_git_mock
    ) -> None:
        run_git_mock.side_effect = [
            json.dumps(
                {
                    "refs": {
                        "base": "base-sha",
                        "upstream": "upstream-sha",
                        "local": "local-sha",
                    }
                }
            ),
            "2026-08-28T18:05:47Z\n2026-08-27T18:05:47Z",
        ]
        inventory = report.snapshot_inventory(
            "upstream/openai-codex/snapshot/inventory.json",
            "refs/remotes/origin/main",
        )

        self.assertEqual(
            inventory["recordedAt"],
            report.parse_timestamp("2026-08-27T18:05:47Z"),
        )
        self.assertEqual(
            run_git_mock.call_args_list,
            [
                call(
                    "show",
                    "refs/remotes/origin/main:upstream/openai-codex/snapshot/inventory.json",
                ),
                call(
                    "log",
                    "--first-parent",
                    "--diff-filter=A",
                    "--format=%cI",
                    "refs/remotes/origin/main",
                    "--",
                    "upstream/openai-codex/snapshot/inventory.json",
                ),
            ],
        )

    @patch("upstream_convergence_report.run_git")
    @patch("upstream_convergence_report.build_candidate_gate")
    @patch("upstream_convergence_report.collect_cycle_telemetry")
    def test_builds_compact_report_without_alerts(
        self,
        telemetry_mock,
        gate_mock,
        run_git_mock,
    ) -> None:
        run_git_mock.side_effect = ["10", (NOW - timedelta(hours=12)).isoformat()]
        gate_mock.return_value = candidate_gate()
        telemetry_mock.return_value = cycle_telemetry()

        compact, markdown = report.build_report(
            inspection_report(),
            NOW,
            NOW - timedelta(hours=6),
            report.DEFAULT_MAX_LAG_COMMITS,
            report.DEFAULT_MAX_MERGE_BASE_AGE_HOURS,
            report.DEFAULT_MAX_REPORT_AGE_HOURS,
        )

        self.assertEqual(
            compact,
            {
                "schemaVersion": 2,
                "generatedAt": "2026-08-22T18:00:00Z",
                "refs": {
                    "base": "base-sha",
                    "upstream": "upstream-sha",
                    "local": "local-sha",
                },
                "counts": {
                    "upstreamLagCommits": 10,
                    "conflicts": 2,
                    "residualLocalInfluence": 7,
                    "laneCounts": {
                        "green_bulk_adopt": 4,
                        "red_manual_review": 1,
                    },
                },
                "agesHours": {
                    "mergeBase": 12.0,
                    "previousSuccessfulReport": 6.0,
                },
                "thresholds": {
                    "maxLagCommits": 50,
                    "maxMergeBaseAgeHours": 24,
                    "maxReportAgeHours": 8,
                },
                "candidateGate": gate_mock.return_value,
                "cycleTelemetry": telemetry_mock.return_value,
                "alerts": [],
            },
        )
        self.assertIn("**Lane counts**", markdown)
        self.assertIn("### Candidate Gate", markdown)
        self.assertIn("### Cycle Telemetry", markdown)

    @patch("upstream_convergence_report.run_git")
    @patch("upstream_convergence_report.build_candidate_gate")
    @patch("upstream_convergence_report.collect_cycle_telemetry")
    def test_reports_lag_snapshot_and_report_freshness_alerts(
        self,
        telemetry_mock,
        gate_mock,
        run_git_mock,
    ) -> None:
        run_git_mock.side_effect = ["51", (NOW - timedelta(hours=25)).isoformat()]
        gate_mock.return_value = candidate_gate(
            due=True,
            reasons=[
                "latest integrated snapshot age 73.0 hours meets 72-hour candidate threshold"
            ],
        )
        telemetry_mock.return_value = cycle_telemetry()

        compact, markdown = report.build_report(
            inspection_report(),
            NOW,
            NOW - timedelta(hours=9),
            report.DEFAULT_MAX_LAG_COMMITS,
            report.DEFAULT_MAX_MERGE_BASE_AGE_HOURS,
            report.DEFAULT_MAX_REPORT_AGE_HOURS,
        )

        self.assertEqual(len(compact["alerts"]), 4)
        self.assertIn("50-commit threshold", markdown)
        self.assertIn("24-hour threshold", markdown)
        self.assertIn("8-hour threshold", markdown)

    @patch("upstream_convergence_report.run_git")
    def test_candidate_gate_enforces_72_hours(self, run_git_mock) -> None:
        run_git_mock.side_effect = ["10"]
        with (
            patch.object(
                report,
                "resolve_latest_integrated_snapshot",
                return_value={
                    "snapshot": "upstream/openai-codex/b89ce9a2-4462b9de",
                    "refs": {
                        "base": "b89ce9a2bcedcfddf3a48f387b7912d602d6d87c",
                        "upstream": "4462b9deef211723b781b426f5e5d36a5777115f",
                        "local": "8add494682f7c0674672e8dc5b38a4565cd7629b",
                    },
                    "recordedAt": "2026-08-19T17:00:00Z",
                },
            ),
            patch.object(report, "commit_exists", return_value=True),
            patch.object(report, "ref_is_ancestor", return_value=True),
        ):
            gate = report.build_candidate_gate("upstream-sha", NOW)

        self.assertTrue(gate["due"])
        self.assertIn("72-hour candidate threshold", gate["reasons"][0])

    @patch("upstream_convergence_report.run_git")
    def test_candidate_gate_enforces_75_commits(self, run_git_mock) -> None:
        run_git_mock.side_effect = ["75"]
        with (
            patch.object(
                report,
                "resolve_latest_integrated_snapshot",
                return_value={
                    "snapshot": "upstream/openai-codex/b89ce9a2-4462b9de",
                    "refs": {
                        "base": "b89ce9a2bcedcfddf3a48f387b7912d602d6d87c",
                        "upstream": "4462b9deef211723b781b426f5e5d36a5777115f",
                        "local": "8add494682f7c0674672e8dc5b38a4565cd7629b",
                    },
                    "recordedAt": "2026-08-22T17:00:00Z",
                },
            ),
            patch.object(report, "commit_exists", return_value=True),
            patch.object(report, "ref_is_ancestor", return_value=True),
        ):
            gate = report.build_candidate_gate("upstream-sha", NOW)

        self.assertTrue(gate["due"])
        self.assertIn("75-commit candidate threshold", gate["reasons"][0])

    @patch("upstream_convergence_report.run_git", return_value="74")
    def test_candidate_gate_is_not_due_below_both_thresholds(
        self, _run_git_mock
    ) -> None:
        with (
            patch.object(
                report,
                "resolve_latest_integrated_snapshot",
                return_value={
                    "snapshot": "upstream/openai-codex/b89ce9a2-4462b9de",
                    "refs": {
                        "base": "base-sha",
                        "upstream": "snapshot-upstream",
                        "local": "local-sha",
                    },
                    "recordedAt": (NOW - timedelta(hours=71, minutes=59))
                    .isoformat()
                    .replace("+00:00", "Z"),
                },
            ),
            patch.object(report, "commit_exists", return_value=True),
            patch.object(report, "ref_is_ancestor", return_value=True),
        ):
            gate = report.build_candidate_gate("upstream-sha", NOW)

        self.assertEqual(gate["status"], "ready")
        self.assertFalse(gate["due"])
        self.assertEqual(gate["reasons"], [])

    def test_unique_ancestry_tip_rejects_ambiguous_snapshot_provenance(self) -> None:
        records = [
            {
                "path": Path("upstream/openai-codex/a"),
                "base": "a",
                "upstream": "u1",
                "local": "l1",
                "recordedAt": NOW,
            },
            {
                "path": Path("upstream/openai-codex/b"),
                "base": "b",
                "upstream": "u2",
                "local": "l2",
                "recordedAt": NOW,
            },
        ]

        with (
            patch.object(report, "ref_is_ancestor", return_value=False),
            self.assertRaisesRegex(ValueError, "ambiguous snapshot provenance"),
        ):
            report.unique_ancestry_tip(records)

    def test_unique_ancestry_tip_deduplicates_retried_upstream_snapshot(self) -> None:
        older = {
            "path": "upstream/openai-codex/older",
            "upstream": "same-upstream",
            "recordedAt": NOW - timedelta(hours=1),
        }
        newer = {
            "path": "upstream/openai-codex/newer",
            "upstream": "same-upstream",
            "recordedAt": NOW,
        }

        self.assertEqual(report.unique_ancestry_tip([older, newer]), newer)

    def test_resolves_latest_snapshot_from_integration_ref_ancestry(self) -> None:
        paths = "\n".join(
            [
                "upstream/openai-codex/old/inventory.json",
                "upstream/openai-codex/new/inventory.json",
                "upstream/openai-codex/new/inventory.md",
            ]
        )
        records = {
            "upstream/openai-codex/old/inventory.json": {
                "path": "upstream/openai-codex/old",
                "base": "base-old",
                "upstream": "upstream-old",
                "local": "local-old",
                "recordedAt": NOW - timedelta(days=2),
            },
            "upstream/openai-codex/new/inventory.json": {
                "path": "upstream/openai-codex/new",
                "base": "base-new",
                "upstream": "upstream-new",
                "local": "local-new",
                "recordedAt": NOW - timedelta(hours=2),
            },
        }

        def is_ancestor(ancestor: str, descendant: str) -> bool:
            return ancestor == "upstream-old" and descendant == "upstream-new"

        with (
            patch.object(report, "commit_exists", return_value=True),
            patch.object(report, "run_git", return_value=paths),
            patch.object(
                report,
                "snapshot_inventory",
                side_effect=lambda path, _ref: records[path],
            ),
            patch.object(report, "ref_is_ancestor", side_effect=is_ancestor),
        ):
            latest = report.resolve_latest_integrated_snapshot("integration-ref")

        self.assertEqual(
            latest,
            {
                "snapshot": "upstream/openai-codex/new",
                "refs": {
                    "base": "base-new",
                    "upstream": "upstream-new",
                    "local": "local-new",
                },
                "recordedAt": "2026-08-22T16:00:00Z",
            },
        )

    @patch("upstream_convergence_report.run_git")
    def test_snapshot_inventory_names_missing_integration_history(
        self, run_git_mock
    ) -> None:
        run_git_mock.side_effect = [
            json.dumps(
                {
                    "refs": {
                        "base": "base-sha",
                        "upstream": "upstream-sha",
                        "local": "local-sha",
                    }
                }
            ),
            "",
        ]

        with self.assertRaisesRegex(
            ValueError, "snapshot is not present in first-parent integration history"
        ):
            report.snapshot_inventory(
                "upstream/openai-codex/snapshot/inventory.json",
                "integration-ref",
            )

    @patch("upstream_convergence_report.run_git")
    @patch("upstream_convergence_report.build_candidate_gate")
    @patch("upstream_convergence_report.collect_cycle_telemetry")
    def test_candidate_provenance_error_preserves_bounded_report(
        self,
        telemetry_mock,
        gate_mock,
        run_git_mock,
    ) -> None:
        run_git_mock.side_effect = ["10", (NOW - timedelta(hours=12)).isoformat()]
        gate_mock.side_effect = ValueError("upstream provenance moved")
        telemetry_mock.return_value = cycle_telemetry()

        compact, markdown = report.build_report(
            inspection_report(),
            NOW,
            NOW - timedelta(hours=6),
            report.DEFAULT_MAX_LAG_COMMITS,
            report.DEFAULT_MAX_MERGE_BASE_AGE_HOURS,
            report.DEFAULT_MAX_REPORT_AGE_HOURS,
        )

        self.assertEqual(
            compact["candidateGate"],
            {
                "status": "error",
                "due": True,
                "thresholds": {"maxAgeHours": 72, "maxUpstreamCommits": 75},
                "latestIntegratedSnapshot": None,
                "reasons": ["candidate gate provenance is unavailable"],
                "error": "upstream provenance moved",
            },
        )
        self.assertIn("candidate gate error: upstream provenance moved", compact["alerts"])
        self.assertIn("**Status**: `error`", markdown)

    def test_rejects_missing_refs(self) -> None:
        with self.assertRaisesRegex(ValueError, "missing refs"):
            report.build_report({}, NOW, None, 50, 24, 8)

    def test_collect_cycle_telemetry_requires_explicit_accounting(self) -> None:
        with patch.dict(report.os.environ, telemetry_env(), clear=True):
            telemetry = report.collect_cycle_telemetry(NOW)

        self.assertEqual(
            telemetry["modelTelemetry"],
            {
                "calls": 0,
                "promptTokens": 0,
                "completionTokens": 0,
                "totalTokens": 0,
                "accountingConfidence": "explicit_zero",
                "modelFree": True,
            },
        )
        self.assertEqual(telemetry["run"]["cycleId"], "123456:2:schedule")

    def test_collect_cycle_telemetry_fails_closed_when_identity_missing(self) -> None:
        with patch.dict(report.os.environ, {}, clear=True):
            with self.assertRaisesRegex(ValueError, "missing inspection telemetry"):
                report.collect_cycle_telemetry(NOW)

    @patch("upstream_convergence_report.run_git")
    @patch("upstream_convergence_report.build_candidate_gate")
    @patch("upstream_convergence_report.collect_cycle_telemetry")
    def test_main_writes_compact_artifact(
        self,
        telemetry_mock,
        gate_mock,
        run_git_mock,
    ) -> None:
        run_git_mock.side_effect = ["10", (NOW - timedelta(hours=12)).isoformat()]
        gate_mock.return_value = candidate_gate()
        telemetry_mock.return_value = cycle_telemetry()
        with (
            patch(
                "upstream_convergence_report.datetime", wraps=datetime
            ) as datetime_mock,
            tempfile.TemporaryDirectory() as temp_dir,
        ):
            datetime_mock.now.return_value = NOW
            report_path = Path(temp_dir) / "report.json"
            output_path = Path(temp_dir) / "summary.json"
            report_path.write_text(json.dumps(inspection_report()), encoding="utf-8")

            with patch.dict(report.os.environ, telemetry_env(), clear=True):
                exit_code = report.main(
                    [
                        "--report",
                        str(report_path),
                        "--output",
                        str(output_path),
                        "--previous-success-at",
                        "2026-08-22T12:00:00Z",
                    ]
                )

            self.assertEqual(exit_code, 0)
            self.assertEqual(json.loads(output_path.read_text())["alerts"], [])


if __name__ == "__main__":
    unittest.main()
