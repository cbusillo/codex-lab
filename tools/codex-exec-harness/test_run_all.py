#!/usr/bin/env python3
"""Regression tests for the Codex exec harness runner."""

import importlib.util
import json
import tempfile
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from unittest import mock


RUN_ALL_PATH = Path(__file__).with_name("run_all.py")
SPEC = importlib.util.spec_from_file_location("codex_exec_harness_run_all", RUN_ALL_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {RUN_ALL_PATH}")
RUN_ALL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUN_ALL)


class RunAllReportTest(unittest.TestCase):
    def test_main_writes_aggregate_report(self) -> None:
        summaries = {
            "local-provider-config": {
                "scenario": "local-provider-config",
                "passed": True,
                "returncode": 0,
                "token_usage": {
                    "input_tokens": 10,
                    "cached_input_tokens": 7,
                    "output_tokens": 2,
                    "reasoning_output_tokens": 1,
                    "total_tokens": 12,
                },
            },
            "multi-turn-resume": {
                "scenario": "multi-turn-resume",
                "passed": True,
                "returncode": 0,
                "token_usage": {
                    "input_tokens": 20,
                    "cached_input_tokens": 9,
                    "output_tokens": 3,
                    "reasoning_output_tokens": 0,
                    "total_tokens": 23,
                },
            },
            "token-usage-report": {
                "scenario": "token-usage-report",
                "passed": True,
                "returncode": 0,
                "token_usage": {
                    "input_tokens": 280,
                    "cached_input_tokens": 160,
                    "output_tokens": 55,
                    "reasoning_output_tokens": 13,
                    "total_tokens": 335,
                },
            },
        }

        def fake_run(command, **_kwargs):
            if command[:3] == ["git", "rev-parse", "HEAD"]:
                return RUN_ALL.subprocess.CompletedProcess(command, 0, stdout="abc123\n")
            scenario = Path(command[2]).stem
            summary = summaries.get(scenario, {"scenario": scenario, "passed": True})
            return RUN_ALL.subprocess.CompletedProcess(
                command,
                0,
                stdout=json.dumps(summary),
                stderr="",
            )

        with tempfile.TemporaryDirectory() as tmp:
            report_path = Path(tmp) / "report.json"
            with mock.patch.object(RUN_ALL.subprocess, "run", side_effect=fake_run):
                with redirect_stdout(StringIO()):
                    returncode = RUN_ALL.main(
                        [
                            "--codex-bin",
                            "/tmp/codex",
                            "--output-root",
                            str(Path(tmp) / "runs"),
                            "--report-json",
                            str(report_path),
                        ]
                    )

            report = json.loads(report_path.read_text(encoding="utf-8"))

        self.assertEqual(0, returncode)
        self.assertTrue(report["passed"])
        self.assertFalse(report["partial"])
        self.assertEqual(6, report["scenario_total"])
        self.assertEqual("abc123", report["git_revision"])
        self.assertEqual(
            {
                "input_tokens": 310,
                "cached_input_tokens": 176,
                "output_tokens": 60,
                "reasoning_output_tokens": 14,
                "total_tokens": 370,
            },
            report["token_usage"],
        )


if __name__ == "__main__":
    unittest.main()
