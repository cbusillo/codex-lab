#!/usr/bin/env python3
"""Regression tests for the Codex exec harness."""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


HARNESS_PATH = Path(__file__).with_name("harness.py")
SPEC = importlib.util.spec_from_file_location("codex_exec_harness", HARNESS_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {HARNESS_PATH}")
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


class HarnessSafetyTest(unittest.TestCase):
    def test_make_paths_sanitizes_scenario_name(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = HARNESS.make_paths(Path(tmp), "../../bad name")

            self.assertEqual(paths.run_dir.parent, Path(tmp))
            self.assertIn("bad-name", paths.run_dir.name)

    def test_materialize_workspace_rejects_escaping_paths(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = HARNESS.make_paths(Path(tmp), "escape")

            with self.assertRaisesRegex(HARNESS.HarnessError, "escapes workspace"):
                HARNESS.materialize_workspace(
                    {"files": {"../outside.txt": "nope"}}, paths
                )

    def test_fake_responses_rejects_empty_responses(self) -> None:
        with self.assertRaisesRegex(HARNESS.HarnessError, "must not be empty"):
            HARNESS.FakeResponsesServer({"responses": []})

    def test_response_sse_body_forwards_fixture_usage(self) -> None:
        body = HARNESS.response_sse_body(
            {
                "response_id": "resp-usage",
                "usage": {
                    "input_tokens": 100,
                    "cached_input_tokens": 75,
                    "output_tokens": 20,
                    "reasoning_output_tokens": 5,
                },
            }
        )

        completed = sse_payload(body, "response.completed")

        self.assertEqual(
            {
                "id": "resp-usage",
                "usage": {
                    "input_tokens": 100,
                    "input_tokens_details": {"cached_tokens": 75},
                    "output_tokens": 20,
                    "output_tokens_details": {"reasoning_tokens": 5},
                    "total_tokens": 120,
                },
                "output": [],
            },
            completed["response"],
        )

    def test_response_sse_body_rejects_non_integer_fixture_usage(self) -> None:
        with self.assertRaisesRegex(
            HARNESS.HarnessError, "responses_api usage input_tokens must be an integer"
        ):
            HARNESS.response_sse_body(
                {
                    "response_id": "resp-usage",
                    "usage": {"input_tokens": 100.5},
                }
            )

    def test_response_sse_body_preserves_explicit_zero_total_tokens(self) -> None:
        body = HARNESS.response_sse_body(
            {
                "response_id": "resp-usage",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 20,
                    "total_tokens": 0,
                },
            }
        )

        completed = sse_payload(body, "response.completed")

        self.assertEqual(0, completed["response"]["usage"]["total_tokens"])

    def test_same_thread_id_expectation_rejects_fresh_thread(self) -> None:
        failures = HARNESS.evaluate_expectations(
            {"expect": {"same_thread_id": True}},
            {
                "returncode": 0,
                "events": [],
                "event_types": {},
                "thread_id": "thread-one",
                "turns": [
                    {"thread_id": "thread-one"},
                    {"thread_id": "thread-two"},
                ],
            },
            [],
        )

        self.assertEqual(
            ["turn 1: expected thread_id 'thread-one', found 'thread-two'"],
            failures,
        )

    def test_token_usage_expectations_accept_bounds_and_cache_ratio(self) -> None:
        failures = HARNESS.evaluate_expectations(
            {
                "expect": {
                    "turns": [
                        {
                            "token_usage": {
                                "input_tokens_min": 90,
                                "input_tokens_max": 110,
                                "cached_input_tokens_min": 70,
                                "cache_ratio_min": 0.7,
                            }
                        }
                    ]
                }
            },
            {
                "returncode": 0,
                "events": [],
                "event_types": {},
                "turns": [
                    {
                        "token_usage": {
                            "input_tokens": 100,
                            "cached_input_tokens": 75,
                            "output_tokens": 10,
                            "reasoning_output_tokens": 0,
                            "total_tokens": 110,
                        }
                    }
                ],
            },
            [],
        )

        self.assertEqual([], failures)

    def test_token_usage_expectations_report_prompt_bloat_and_cache_miss(self) -> None:
        failures = HARNESS.evaluate_expectations(
            {
                "expect": {
                    "turns": [
                        {
                            "token_usage": {
                                "input_tokens_max": 100,
                                "cached_input_tokens_min": 80,
                                "cache_ratio_min": 0.5,
                            }
                        }
                    ]
                }
            },
            {
                "returncode": 0,
                "events": [],
                "event_types": {},
                "turns": [
                    {
                        "token_usage": {
                            "input_tokens": 200,
                            "cached_input_tokens": 50,
                            "output_tokens": 10,
                            "reasoning_output_tokens": 0,
                            "total_tokens": 210,
                        }
                    }
                ],
            },
            [],
        )

        self.assertEqual(
            [
                "turn 0.token_usage: expected input_tokens <= 100, found 200",
                "turn 0.token_usage: expected cached_input_tokens >= 80, found 50",
                "turn 0.token_usage: expected cache_ratio >= 0.5, found 0.250",
            ],
            failures,
        )

    def test_token_usage_expectations_reject_non_numeric_bounds(self) -> None:
        with self.assertRaisesRegex(
            HARNESS.HarnessError,
            "turn 0.token_usage.input_tokens_min must be an integer",
        ):
            HARNESS.evaluate_expectations(
                {"expect": {"turns": [{"token_usage": {"input_tokens_min": "many"}}]}},
                {"returncode": 0, "events": [], "event_types": {}, "turns": [{}]},
                [],
            )

    def test_token_usage_expectations_reject_float_integer_bounds(self) -> None:
        with self.assertRaisesRegex(
            HARNESS.HarnessError,
            "turn 0.token_usage.input_tokens_min must be an integer",
        ):
            HARNESS.evaluate_expectations(
                {"expect": {"turns": [{"token_usage": {"input_tokens_min": 1.5}}]}},
                {"returncode": 0, "events": [], "event_types": {}, "turns": [{}]},
                [],
            )

    def test_response_prefix_assertion_compares_captured_requests(self) -> None:
        failures = HARNESS.evaluate_expectations(
            {
                "expect": {
                    "responses": [
                        {
                            "request": 1,
                            "scope": "input",
                            "prefix_matches_request": 0,
                            "prefix_length": 12,
                        }
                    ]
                }
            },
            {"returncode": 0, "events": [], "event_types": {}, "turns": []},
            [
                {"body": {"input": "stable-prefix first"}},
                {"body": {"input": "stable-prefix second"}},
            ],
        )

        self.assertEqual([], failures)

    def test_response_prefix_assertion_reports_mismatch(self) -> None:
        failures = HARNESS.evaluate_expectations(
            {
                "expect": {
                    "responses": [
                        {
                            "request": 1,
                            "scope": "input",
                            "prefix_matches_request": 0,
                            "prefix_length": 6,
                        }
                    ]
                }
            },
            {"returncode": 0, "events": [], "event_types": {}, "turns": []},
            [
                {"body": {"input": "first request"}},
                {"body": {"input": "second request"}},
            ],
        )

        self.assertEqual(
            [
                "responses[1].input: expected first 6 characters to match "
                "responses[0].input"
            ],
            failures,
        )

    def test_response_prefix_assertion_rejects_self_reference(self) -> None:
        with self.assertRaisesRegex(
            HARNESS.HarnessError,
            r"responses\[0\]\.input prefix_matches_request cannot reference itself",
        ):
            HARNESS.evaluate_expectations(
                {
                    "expect": {
                        "responses": [
                            {
                                "request": 0,
                                "scope": "input",
                                "prefix_matches_request": 0,
                                "prefix_length": 6,
                            }
                        ]
                    }
                },
                {"returncode": 0, "events": [], "event_types": {}, "turns": []},
                [{"body": {"input": "same request"}}],
            )

    def test_response_prefix_assertion_rejects_non_integer_reference(self) -> None:
        with self.assertRaisesRegex(
            HARNESS.HarnessError,
            r"responses\[1\]\.input.prefix_matches_request must be an integer",
        ):
            HARNESS.evaluate_expectations(
                {
                    "expect": {
                        "responses": [
                            {
                                "request": 1,
                                "scope": "input",
                                "prefix_matches_request": "first",
                                "prefix_length": 6,
                            }
                        ]
                    }
                },
                {"returncode": 0, "events": [], "event_types": {}, "turns": []},
                [
                    {"body": {"input": "first request"}},
                    {"body": {"input": "second request"}},
                ],
            )

    def test_response_prefix_assertion_reports_missing_scope(self) -> None:
        failures = HARNESS.evaluate_expectations(
            {
                "expect": {
                    "responses": [
                        {
                            "request": 1,
                            "scope": "inputs",
                            "prefix_matches_request": 0,
                            "prefix_length": 6,
                        }
                    ]
                }
            },
            {"returncode": 0, "events": [], "event_types": {}, "turns": []},
            [
                {"body": {"input": "first request"}},
                {"body": {"input": "second request"}},
            ],
        )

        self.assertEqual(["responses[1].inputs: missing prefix subject"], failures)

    def test_run_codex_preserves_artifacts_on_timeout(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = HARNESS.make_paths(Path(tmp), "timeout")
            paths.workspace.mkdir(parents=True)
            artifact_dir = paths.artifacts
            artifact_dir.mkdir(parents=True)

            result = HARNESS.run_codex(
                [
                    "python3",
                    "-c",
                    "import sys,time; print('{\"type\":\"turn.started\"}', flush=True); print('before sleep', file=sys.stderr, flush=True); time.sleep(30)",
                ],
                {"timeout_seconds": 1},
                paths,
                artifact_dir,
            )

            self.assertEqual(124, result["returncode"])
            self.assertTrue(result["timed_out"])
            self.assertTrue((artifact_dir / "stdout.jsonl").exists())
            self.assertTrue((artifact_dir / "stderr.log").exists())
            self.assertIn("turn.started", (artifact_dir / "stdout.jsonl").read_text())
            self.assertIn("timed out", (artifact_dir / "stderr.log").read_text())

    def test_token_usage_snapshot_from_events_uses_last_turn_completed_usage(self) -> None:
        usage = HARNESS.token_usage_snapshot_from_events(
            [
                {"type": "turn.started"},
                {
                    "type": "turn.completed",
                    "usage": {
                        "input_tokens": 10,
                        "cached_input_tokens": 4,
                        "output_tokens": 3,
                        "reasoning_output_tokens": 2,
                    },
                },
                {
                    "type": "turn.completed",
                    "usage": {
                        "input_tokens": 7,
                        "cached_input_tokens": 5,
                        "output_tokens": 1,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 9,
                    },
                },
            ]
        )

        self.assertEqual(
            {
                "input_tokens": 7,
                "cached_input_tokens": 5,
                "output_tokens": 1,
                "reasoning_output_tokens": 0,
                "total_tokens": 9,
            },
            usage,
        )

    def test_subtract_token_usage_derives_turn_delta(self) -> None:
        delta = HARNESS.subtract_token_usage(
            {
                "input_tokens": 10,
                "cached_input_tokens": 6,
                "output_tokens": 3,
                "reasoning_output_tokens": 1,
                "total_tokens": 13,
            },
            {
                "input_tokens": 7,
                "cached_input_tokens": 2,
                "output_tokens": 1,
                "reasoning_output_tokens": 1,
                "total_tokens": 8,
            },
        )

        self.assertEqual(
            {
                "input_tokens": 3,
                "cached_input_tokens": 4,
                "output_tokens": 2,
                "reasoning_output_tokens": 0,
                "total_tokens": 5,
            },
            delta,
        )


def sse_payload(body: str, event_type: str) -> dict:
    for chunk in body.strip().split("\n\n"):
        lines = chunk.splitlines()
        if lines[0] != f"event: {event_type}":
            continue
        return json.loads(lines[1].removeprefix("data: "))
    raise AssertionError(f"missing SSE event {event_type}")


if __name__ == "__main__":
    unittest.main()
