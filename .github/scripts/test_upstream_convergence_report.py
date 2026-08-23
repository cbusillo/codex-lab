import json
import tempfile
import unittest
from datetime import datetime
from datetime import timedelta
from datetime import timezone
from pathlib import Path
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


class TestUpstreamConvergenceReport(unittest.TestCase):
    @patch("upstream_convergence_report.run_git")
    def test_builds_compact_report_without_alerts(self, run_git_mock) -> None:
        run_git_mock.side_effect = ["10", (NOW - timedelta(hours=12)).isoformat()]

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
                "schemaVersion": 1,
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
                "alerts": [],
            },
        )
        self.assertIn("**Lane counts**", markdown)

    @patch("upstream_convergence_report.run_git")
    def test_reports_lag_snapshot_and_report_freshness_alerts(self, run_git_mock) -> None:
        run_git_mock.side_effect = ["51", (NOW - timedelta(hours=25)).isoformat()]

        compact, markdown = report.build_report(
            inspection_report(),
            NOW,
            NOW - timedelta(hours=9),
            report.DEFAULT_MAX_LAG_COMMITS,
            report.DEFAULT_MAX_MERGE_BASE_AGE_HOURS,
            report.DEFAULT_MAX_REPORT_AGE_HOURS,
        )

        self.assertEqual(len(compact["alerts"]), 3)
        self.assertIn("50-commit threshold", markdown)
        self.assertIn("24-hour threshold", markdown)
        self.assertIn("8-hour threshold", markdown)

    def test_rejects_missing_refs(self) -> None:
        with self.assertRaisesRegex(ValueError, "missing refs"):
            report.build_report({}, NOW, None, 50, 24, 8)

    @patch("upstream_convergence_report.run_git")
    def test_main_writes_compact_artifact(self, run_git_mock) -> None:
        run_git_mock.side_effect = ["10", (NOW - timedelta(hours=12)).isoformat()]
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
