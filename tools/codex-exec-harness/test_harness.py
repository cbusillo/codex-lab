#!/usr/bin/env python3
"""Regression tests for the Codex exec harness."""

import importlib.util
import json
import tempfile
import unittest
import unittest.mock
from argparse import Namespace
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path


HARNESS_PATH = Path(__file__).with_name("harness.py")
SPEC = importlib.util.spec_from_file_location("codex_exec_harness", HARNESS_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {HARNESS_PATH}")
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


class HarnessSafetyTest(unittest.TestCase):
    def test_auto_review_run_target_classification(self) -> None:
        active_branch = "plan/auto-review-proof-loop-mvp"
        active_head = "abc123"

        self.assertEqual(
            "current",
            HARNESS.classify_auto_review_run(
                {"target": {"branch": active_branch, "head_sha": active_head}},
                active_branch,
                active_head,
            ),
        )
        self.assertEqual(
            "detached",
            HARNESS.classify_auto_review_run({}, active_branch, active_head),
        )
        self.assertEqual(
            "stale",
            HARNESS.classify_auto_review_run(
                {"target": {"branch": active_branch, "head_sha": "old123"}},
                active_branch,
                active_head,
            ),
        )
        self.assertEqual(
            "detached",
            HARNESS.classify_auto_review_run(
                {"target": {"branch": "auto-review", "head_sha": active_head}},
                active_branch,
                active_head,
            ),
        )

    def test_auto_review_clean_or_stale_runs_render_no_summary(self) -> None:
        active_branch = "main"
        active_head = "abc123"

        self.assertEqual(
            "",
            HARNESS.render_auto_review_summary(
                {"target": {"branch": active_branch, "head_sha": active_head}, "findings": []},
                active_branch,
                active_head,
            ),
        )
        self.assertEqual(
            "",
            HARNESS.render_auto_review_summary(
                {
                    "target": {"branch": active_branch, "head_sha": "old123"},
                    "findings": [{"id": "f1", "title": "stale", "priority": "P1"}],
                },
                active_branch,
                active_head,
            ),
        )

    def test_auto_review_current_summary_and_bounded_detail(self) -> None:
        run = {
            "target": {"branch": "main", "head_sha": "abc123"},
            "findings": [
                {
                    "id": "f1",
                    "priority": "P1",
                    "title": "Use current head",
                    "location": "src/lib.rs:12",
                    "body": "x" * 200,
                }
            ],
        }

        self.assertEqual(
            "[P1] f1: Use current head (src/lib.rs:12)",
            HARNESS.render_auto_review_summary(run, "main", "abc123"),
        )

        detail = HARNESS.auto_review_finding_detail(run, "f1", max_bytes=80)

        self.assertEqual("f1", detail["finding_id"])
        self.assertEqual(80, detail["max_bytes"])
        self.assertTrue(detail["truncated"])
        self.assertEqual(len(detail["content"].encode("utf-8")), detail["bytes"])
        self.assertLessEqual(len(detail["content"].encode("utf-8")), 80)

    def test_auto_review_summary_is_bounded(self) -> None:
        run = {
            "target": {"branch": "main", "head_sha": "abc123"},
            "findings": [
                {
                    "id": f"f{index}",
                    "priority": "P2",
                    "title": "long title " * 80,
                    "location": "long/path/" * 80,
                }
                for index in range(30)
            ],
        }

        summary = HARNESS.render_auto_review_summary(run, "main", "abc123")

        self.assertLessEqual(
            len(summary.encode("utf-8")), HARNESS.AUTO_REVIEW_SUMMARY_MAX_BYTES
        )
        self.assertIn("more finding(s) omitted", summary)

    def test_auto_review_summary_marks_byte_budget_omissions(self) -> None:
        run = {
            "target": {"branch": "main", "head_sha": "abc123"},
            "findings": [
                {
                    "id": f"f{index}",
                    "priority": "P2",
                    "title": "title " * 60,
                    "location": "src/very/long/path/" * 20,
                }
                for index in range(20)
            ],
        }

        summary = HARNESS.render_auto_review_summary(run, "main", "abc123")

        self.assertLessEqual(
            len(summary.encode("utf-8")), HARNESS.AUTO_REVIEW_SUMMARY_MAX_BYTES
        )
        self.assertIn("more finding(s) omitted", summary)
        self.assertNotIn("f19", summary)

    def test_auto_review_summary_marks_count_cap_omissions(self) -> None:
        run = {
            "target": {"branch": "main", "head_sha": "abc123"},
            "findings": [
                {
                    "id": f"f{index}",
                    "priority": "P2",
                    "title": "short",
                    "location": f"src/main.rs:{index}",
                }
                for index in range(21)
            ],
        }

        summary = HARNESS.render_auto_review_summary(run, "main", "abc123")

        self.assertIn("[P2] f19: short (src/main.rs:19)", summary)
        self.assertNotIn("f20", summary)
        self.assertIn("... 1 more finding(s) omitted", summary)

    def test_auto_review_summary_does_not_mark_fully_rendered_findings(self) -> None:
        run = {
            "target": {"branch": "main", "head_sha": "abc123"},
            "findings": [
                {
                    "id": "f1",
                    "priority": "P2",
                    "title": "one",
                    "location": "src/main.rs:1",
                },
                {
                    "id": "f2",
                    "priority": "P3",
                    "title": "two",
                    "location": "src/main.rs:2",
                },
            ],
        }

        summary = HARNESS.render_auto_review_summary(run, "main", "abc123")

        self.assertEqual(
            "[P2] f1: one (src/main.rs:1)\n[P3] f2: two (src/main.rs:2)",
            summary,
        )

    def test_auto_review_detail_matches_numeric_finding_id(self) -> None:
        detail = HARNESS.auto_review_finding_detail(
            {"findings": [{"id": 7, "title": "numeric id"}]},
            "7",
            max_bytes=1_000,
        )

        self.assertEqual("7", detail["finding_id"])
        self.assertFalse(detail["truncated"])

    def test_auto_review_run_recovers_after_restart(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sidecar_root = Path(tmp)
            run = {
                "run_id": "run-1",
                "target": {"branch": "main", "head_sha": "abc123"},
                "findings": [
                    {
                        "id": "f1",
                        "priority": "P2",
                        "title": "Recovered finding",
                        "location": "src/main.rs:4",
                    }
                ],
            }

            path = HARNESS.save_auto_review_run(sidecar_root, run)
            recovered = HARNESS.load_auto_review_run(sidecar_root, "run-1")

        self.assertEqual(path.name, "run-1.json")
        self.assertEqual(
            "current", HARNESS.classify_auto_review_run(recovered, "main", "abc123")
        )
        self.assertEqual(
            "[P2] f1: Recovered finding (src/main.rs:4)",
            HARNESS.render_auto_review_summary(recovered, "main", "abc123"),
        )

    def test_auto_review_run_id_rejects_path_traversal(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            sidecar_root = Path(tmp)
            with self.assertRaisesRegex(
                HARNESS.HarnessError,
                "auto review run_id contains unsafe path characters",
            ):
                HARNESS.save_auto_review_run(
                    sidecar_root,
                    {"run_id": "../outside", "target": {}, "findings": []},
                )
            with self.assertRaisesRegex(
                HARNESS.HarnessError,
                "auto review run_id contains unsafe path characters",
            ):
                HARNESS.load_auto_review_run(sidecar_root, "/tmp/outside")

    def test_auto_review_missing_run_id_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(
                HARNESS.HarnessError, "unknown auto review run id: missing"
            ):
                HARNESS.load_auto_review_run(Path(tmp), "missing")

    def test_auto_review_unknown_detail_id_is_error(self) -> None:
        with self.assertRaisesRegex(
            HARNESS.HarnessError, "unknown auto review finding id: f2"
        ):
            HARNESS.auto_review_finding_detail(
                {"findings": [{"id": "f1", "title": "one"}]},
                "f2",
                max_bytes=100,
            )

    def test_make_paths_sanitizes_scenario_name(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = HARNESS.make_paths(Path(tmp), "../../bad name")

            self.assertEqual(paths.run_dir.parent, Path(tmp))
            self.assertIn("bad-name", paths.run_dir.name)

    def test_run_scenario_resolves_relative_output_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            scenario_path = Path(tmp) / "scenario.json"
            output_root = Path(tmp) / "runs"
            scenario_path.write_text(
                json.dumps(
                    {
                        "name": "relative-output-root",
                        "prompt": "hi",
                        "responses_api": {"responses": [{}]},
                    }
                ),
                encoding="utf-8",
            )

            paths = HARNESS.make_paths(output_root, "relative-output-root")
            with unittest.mock.patch.object(HARNESS, "make_paths", return_value=paths) as make:
                with unittest.mock.patch.object(HARNESS, "materialize_workspace"):
                    with unittest.mock.patch.object(HARNESS, "save_config"):
                        with unittest.mock.patch.object(
                            HARNESS,
                            "run_turns",
                            return_value={
                                "returncode": 0,
                                "events": [],
                                "event_types": {},
                                "turns": [],
                                "thread_id": None,
                                "token_usage": HARNESS.empty_token_usage(),
                            },
                        ):
                            with unittest.mock.patch.object(
                                HARNESS, "evaluate_expectations", return_value=[]
                            ):
                                with redirect_stdout(StringIO()):
                                    HARNESS.run_scenario(
                                        Namespace(
                                            scenario=str(scenario_path),
                                            codex_bin="/tmp/codex",
                                            output_root="relative-runs",
                                        )
                                    )

            self.assertTrue(make.call_args.args[0].is_absolute())

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
                    "token_usage": {
                        "input_tokens": 100,
                        "cached_input_tokens": 75,
                        "output_tokens": 10,
                        "reasoning_output_tokens": 0,
                        "total_tokens": 110,
                    },
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
                "token_usage": {
                    "input_tokens": 100,
                    "cached_input_tokens": 75,
                    "output_tokens": 10,
                    "reasoning_output_tokens": 0,
                    "total_tokens": 110,
                },
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

    def test_token_usage_snapshot_expectations_are_evaluated(self) -> None:
        failures = HARNESS.evaluate_expectations(
            {
                "expect": {
                    "turns": [
                        {
                            "token_usage_snapshot": {
                                "input_tokens": 12,
                                "cached_input_tokens": 4,
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
                        "token_usage_snapshot": {
                            "input_tokens": 7,
                            "cached_input_tokens": 4,
                            "output_tokens": 1,
                            "reasoning_output_tokens": 0,
                            "total_tokens": 8,
                        }
                    }
                ],
            },
            [],
        )

        self.assertEqual(
            ["turn 0.token_usage_snapshot: expected input_tokens 12, found 7"],
            failures,
        )

    def test_aggregate_token_usage_expectations_are_evaluated(self) -> None:
        failures = HARNESS.evaluate_expectations(
            {"expect": {"token_usage": {"total_tokens": 99}}},
            {
                "returncode": 0,
                "events": [],
                "event_types": {},
                "token_usage": {
                    "input_tokens": 10,
                    "cached_input_tokens": 0,
                    "output_tokens": 1,
                    "reasoning_output_tokens": 0,
                    "total_tokens": 11,
                },
                "turns": [],
            },
            [],
        )

        self.assertEqual(
            ["token_usage: expected total_tokens 99, found 11"],
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
