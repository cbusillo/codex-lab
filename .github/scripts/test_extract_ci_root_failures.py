import json
import subprocess
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("extract_ci_root_failures.py")
sys.path.insert(0, str(SCRIPT.parent))

from extract_ci_root_failures import bounded_report, extract_root_failures


class ExtractCiRootFailuresTest(unittest.TestCase):
    def test_required_and_fan_in_aggregators_are_suppressed(self) -> None:
        report = extract_root_failures(
            {
                "jobs": [
                    {"id": "required", "name": "CI required", "conclusion": "failure"},
                    {
                        "id": 17,
                        "name": "Rust unit tests",
                        "conclusion": "failure",
                    },
                    {
                        "id": 18,
                        "name": "Full CI results",
                        "conclusion": "cancelled",
                    },
                ]
            }
        )

        self.assertEqual(report["summary"]["aggregators_suppressed"], 2)
        self.assertEqual(report["summary"]["failed_aggregators_suppressed"], 1)
        self.assertEqual([failure["root"] for failure in report["failures"]], ["Rust unit tests"])

    def test_failed_step_is_the_root_and_cascade_skips_are_ignored(self) -> None:
        report = extract_root_failures(
            {
                "jobs": [
                    {
                        "databaseId": 1,
                        "name": "rust-ci / Rust workspace compile check",
                        "conclusion": "failure",
                        "steps": [
                            {"name": "Checkout", "conclusion": "success"},
                            {
                                "name": "Compile Rust workspace and tests",
                                "conclusion": "failure",
                            },
                            {"name": "Post cache", "conclusion": "skipped"},
                        ],
                    },
                    {
                        "databaseId": 2,
                        "name": "rust-ci / Argument comment lint package",
                        "conclusion": "skipped",
                        "steps": [],
                    },
                    {
                        "databaseId": 3,
                        "name": "rust-ci / CI results (required)",
                        "conclusion": "failure",
                        "steps": [
                            {"name": "Summarize", "conclusion": "failure"}
                        ],
                    },
                ]
            }
        )

        self.assertEqual(
            [failure["root"] for failure in report["failures"]],
            ["Compile Rust workspace and tests"],
        )
        self.assertEqual(report["summary"]["aggregators_suppressed"], 1)
        self.assertEqual(report["summary"]["non_actionable_ignored"], 1)

    def test_pending_jobs_and_steps_do_not_hide_completed_failure(self) -> None:
        report = extract_root_failures(
            {
                "jobs": [
                    {
                        "databaseId": 1,
                        "name": "Pending matrix job",
                        "conclusion": "",
                        "steps": [{"name": "Run tests", "conclusion": None}],
                    },
                    {
                        "databaseId": 2,
                        "name": "Completed job",
                        "conclusion": "failure",
                        "steps": [
                            {"name": "Setup", "conclusion": None},
                            {"name": "Run tests", "conclusion": "failure"},
                        ],
                    },
                ]
            }
        )

        self.assertEqual([failure["root"] for failure in report["failures"]], ["Run tests"])

    def test_cancelled_jobs_are_fallback_after_failed_aggregator_is_suppressed(self) -> None:
        report = extract_root_failures(
            {
                "jobs": [
                    {"id": "required", "name": "CI required", "conclusion": "failure"},
                    {"id": 2, "name": "macOS tests", "conclusion": "cancelled"},
                ]
            }
        )

        self.assertEqual([failure["root"] for failure in report["failures"]], ["macOS tests"])

    def test_upload_test_results_is_not_treated_as_aggregator(self) -> None:
        report = extract_root_failures(
            {
                "jobs": [
                    {
                        "id": 7,
                        "name": "rust-ci / Upload test results",
                        "conclusion": "failure",
                    }
                ]
            }
        )

        self.assertEqual(
            [failure["root"] for failure in report["failures"]],
            ["rust-ci / Upload test results"],
        )

    def test_explicit_non_aggregator_overrides_name_heuristic(self) -> None:
        report = extract_root_failures(
            {
                "jobs": [
                    {
                        "id": None,
                        "databaseId": 5,
                        "name": "results",
                        "conclusion": "failure",
                        "is_aggregator": False,
                        "steps": [{"conclusion": "failure"}],
                    }
                ]
            }
        )

        self.assertEqual([failure["root"] for failure in report["failures"]], ["results"])
        self.assertEqual(report["failures"][0]["sources"][0]["id"], 5)

    def test_job_and_check_duplicates_share_one_root_and_preserve_sources(self) -> None:
        report = extract_root_failures(
            {
                "jobs": [
                    {
                        "id": 42,
                        "name": "Bazel analysis",
                        "conclusion": "failure",
                        "root_failure": "bazel compile",
                    }
                ],
                "check_runs": [
                    {
                        "id": 9001,
                        "name": "Bazel analysis",
                        "conclusion": "failure",
                        "root_failure": "bazel compile",
                    }
                ],
            }
        )

        self.assertEqual(report["summary"]["duplicates_collapsed"], 1)
        self.assertEqual(
            report["failures"],
            [
                {
                    "root": "bazel compile",
                    "sources": [
                        {
                            "conclusion": "failure",
                            "id": 9001,
                            "kind": "check",
                            "name": "Bazel analysis",
                        },
                        {
                            "conclusion": "failure",
                            "id": 42,
                            "kind": "job",
                            "name": "Bazel analysis",
                        },
                    ],
                }
            ],
        )

    def test_independent_failures_are_sorted_deterministically(self) -> None:
        payload = {
            "jobs": [
                {"id": "z", "name": "Zeta", "conclusion": "timed_out"},
                {"id": "a", "name": "Alpha", "conclusion": "failure"},
            ]
        }
        reversed_payload = {"jobs": list(reversed(payload["jobs"]))}

        first = extract_root_failures(payload)
        second = extract_root_failures(reversed_payload)

        self.assertEqual(first, second)
        self.assertEqual([failure["root"] for failure in first["failures"]], ["Alpha", "Zeta"])
        self.assertEqual(first["failures"][1]["sources"][0]["conclusion"], "timed_out")

    def test_malformed_input_fails_without_a_traceback(self) -> None:
        completed = subprocess.run(
            [sys.executable, str(SCRIPT)],
            input='{"jobs": ["not a result"]}',
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(completed.returncode, 2)
        self.assertEqual(completed.stdout, "")
        self.assertEqual(completed.stderr, "invalid or unbounded CI result input\n")

    def test_root_and_source_bounds_are_reported(self) -> None:
        payload = {
            "jobs": [
                {"id": 1, "name": "Alpha", "conclusion": "failure"},
                {"id": 2, "name": "Beta", "conclusion": "failure"},
                {"id": 3, "name": "Beta", "conclusion": "failure"},
            ]
        }

        report = extract_root_failures(payload, max_roots=1, max_sources_per_root=1)

        self.assertEqual(report["summary"]["total_roots"], 2)
        self.assertEqual(report["summary"]["reported_roots"], 1)
        self.assertEqual(report["summary"]["reported_sources"], 1)
        self.assertTrue(report["summary"]["truncated"])

    def test_output_byte_bound_is_enforced(self) -> None:
        report = extract_root_failures(
            {"jobs": [{"id": 1, "name": "A" * 100, "conclusion": "failure"}]}
        )

        rendered = bounded_report(report, 80)

        self.assertLessEqual(len(rendered.encode("utf-8")), 80)
        self.assertEqual(json.loads(rendered), {"error": "output_limit_exceeded"})


if __name__ == "__main__":
    unittest.main()
