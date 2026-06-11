#!/usr/bin/env python3
"""Regression tests for the Codex exec harness."""

import importlib.util
import argparse
import tempfile
import unittest
import unittest.mock
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
                '{"name":"relative-output-root","prompt":"hi","responses_api":{"responses":[{}]}}',
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
                                        argparse.Namespace(
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

    def test_response_sse_body_rejects_non_object_usage(self) -> None:
        with self.assertRaisesRegex(HARNESS.HarnessError, "usage must be an object"):
            HARNESS.response_sse_body({"usage": 123})

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

    def test_token_usage_expectation_reports_mismatches(self) -> None:
        failures = HARNESS.evaluate_expectations(
            {"expect": {"token_usage": {"cached_input_tokens": 12}}},
            {
                "returncode": 0,
                "events": [],
                "event_types": {},
                "thread_id": "thread-one",
                "token_usage": {"cached_input_tokens": 7},
                "turns": [],
            },
            [],
        )

        self.assertEqual(
            ["run: expected token_usage.cached_input_tokens 12, found 7"],
            failures,
        )


if __name__ == "__main__":
    unittest.main()
