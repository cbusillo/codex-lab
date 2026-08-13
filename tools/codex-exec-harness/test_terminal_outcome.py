#!/usr/bin/env python3
"""Tests for exec-harness terminal outcome classification."""

import importlib.util
import sys
import unittest
from pathlib import Path


HARNESS_DIR = Path(__file__).resolve().parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))

HARNESS_PATH = HARNESS_DIR / "harness.py"
SPEC = importlib.util.spec_from_file_location("codex_exec_harness", HARNESS_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {HARNESS_PATH}")
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


def completed_run(**overrides: object) -> dict[str, object]:
    run: dict[str, object] = {
        "agent_messages": ["done"],
        "stderr": "",
        "stdout_parse_errors": 0,
        "timed_out": False,
        "tool_loop_detected": False,
        "tool_loop_command": None,
    }
    run.update(overrides)
    return run


class TerminalOutcomeTest(unittest.TestCase):
    def test_classification_precedence(self) -> None:
        cases = [
            (
                completed_run(launch_error="binary missing"),
                ("runner_failed", "harness_error"),
            ),
            (
                completed_run(timed_out=True),
                ("runner_failed", "provider_timeout"),
            ),
            (
                completed_run(stderr="connection refused"),
                ("runner_failed", "provider_unavailable"),
            ),
            (
                completed_run(
                    tool_loop_detected=True, tool_loop_command="undefined-helper"
                ),
                ("model_failed", "tool_loop"),
            ),
            (
                completed_run(stdout_parse_errors=1),
                ("runner_failed", "malformed_jsonl"),
            ),
            (
                completed_run(agent_messages=[]),
                ("model_failed", "missing_final_message"),
            ),
        ]

        for run, expected in cases:
            with self.subTest(expected=expected):
                outcome = HARNESS.classify_terminal_outcome(run, [])
                self.assertEqual(expected, (outcome.outcome, outcome.reason))

    def test_policy_failure_and_passed_outcomes(self) -> None:
        failed = HARNESS.classify_terminal_outcome(
            completed_run(), ["expected command was missing"]
        )
        passed = HARNESS.classify_terminal_outcome(completed_run(), [])

        self.assertEqual(
            ("policy_failed", "assertion_mismatch"),
            (failed.outcome, failed.reason),
        )
        self.assertEqual(("passed", "passed"), (passed.outcome, passed.reason))

    def test_terminal_outcome_assertion_uses_final_classification(self) -> None:
        failures: list[str] = []
        outcome = HARNESS.classify_terminal_outcome(
            completed_run(stdout_parse_errors=1), []
        )

        HARNESS.add_terminal_outcome_assertion_failures(
            failures,
            {
                "expect": {
                    "terminal_outcome": {
                        "outcome": "runner_failed",
                        "reason": "malformed_jsonl",
                        "detail_contains": "valid JSONL",
                    }
                }
            },
            outcome,
        )

        self.assertEqual([], failures)

    def test_terminal_outcome_assertion_rejects_unknown_fields(self) -> None:
        with self.assertRaisesRegex(HARNESS.HarnessError, "unsupported fields"):
            HARNESS.add_terminal_outcome_assertion_failures(
                [],
                {"expect": {"terminal_outcome": {"detail": "tool loop"}}},
                HARNESS.model_failed("tool_loop"),
            )

    def test_expected_non_passing_outcome_can_be_a_green_characterization(self) -> None:
        scenario = {
            "expect": {
                "terminal_outcome": {
                    "outcome": "model_failed",
                    "reason": "tool_loop",
                }
            }
        }
        outcome = HARNESS.model_failed("tool_loop")
        failures: list[str] = []

        HARNESS.add_terminal_outcome_assertion_failures(failures, scenario, outcome)

        self.assertEqual([], failures)
        self.assertIsInstance(scenario["expect"]["terminal_outcome"], dict)


if __name__ == "__main__":
    unittest.main()
