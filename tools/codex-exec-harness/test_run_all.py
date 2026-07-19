#!/usr/bin/env python3
"""Regression tests for the Codex exec harness runner."""

import importlib.util
import json
import tempfile
import unittest
from contextlib import redirect_stderr
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from unittest import mock


RUN_ALL_PATH = Path(__file__).with_name("run_all.py")
SPEC = importlib.util.spec_from_file_location(
    "codex_exec_harness_run_all", RUN_ALL_PATH
)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {RUN_ALL_PATH}")
RUN_ALL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUN_ALL)


class RunAllReportTest(unittest.TestCase):
    def test_main_writes_aggregate_report(self) -> None:
        requested_binary = str(Path("/tmp/requested-codex").resolve(strict=False))
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

        provenance_calls = 0

        def fake_run(command, **_kwargs):
            nonlocal provenance_calls
            if command[1] == str(RUN_ALL.PROVENANCE_HELPER):
                provenance_calls += 1
                self.assertEqual(
                    requested_binary, command[command.index("--binary") + 1]
                )
                report = {
                    "schema_version": 1,
                    "status": "current",
                    "binary_path": "/tmp/codex",
                    "expected": {
                        "source_commit": "abc123",
                        "dirty_state": "clean",
                    },
                    "provenance": {
                        "schema_version": 1,
                        "version": "test",
                        "source_commit": "abc123",
                        "dirty_state": "clean",
                        "build_profile": "debug",
                        "build_channel": "dev",
                        "executable_path": "/tmp/codex",
                    },
                    "binary_sha256": "f" * 64,
                    "failures": [],
                }
                return RUN_ALL.subprocess.CompletedProcess(
                    command, 0, stdout=json.dumps(report), stderr=""
                )
            if command[:3] == ["git", "rev-parse", "HEAD"]:
                return RUN_ALL.subprocess.CompletedProcess(
                    command, 0, stdout="abc123\n"
                )
            self.assertEqual("/tmp/codex", command[command.index("--codex-bin") + 1])
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
            with (
                mock.patch.object(
                    RUN_ALL.shutil, "which", return_value=requested_binary
                ),
                mock.patch.object(RUN_ALL.subprocess, "run", side_effect=fake_run),
            ):
                with redirect_stdout(StringIO()):
                    returncode = RUN_ALL.main(
                        [
                            "--codex-bin",
                            "codex",
                            "--output-root",
                            str(Path(tmp) / "runs"),
                            "--report-json",
                            str(report_path),
                        ]
                    )

            report = json.loads(report_path.read_text(encoding="utf-8"))

        self.assertEqual(0, returncode)
        self.assertEqual(1, provenance_calls)
        self.assertTrue(report["passed"])
        self.assertFalse(report["partial"])
        self.assertEqual("/tmp/codex", report["codex_bin"])
        self.assertEqual(17, report["scenario_total"])
        self.assertEqual("abc123", report["git_revision"])
        self.assertEqual("current", report["provenance"]["status"])
        self.assertEqual("f" * 64, report["provenance"]["binary_sha256"])
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

    def test_main_rejects_stale_binary_before_scenarios(self) -> None:
        commands = []

        def fake_run(command, **_kwargs):
            commands.append(command)
            if command[1] == str(RUN_ALL.PROVENANCE_HELPER):
                report = {
                    "schema_version": 1,
                    "status": "stale",
                    "binary_path": "/tmp/codex",
                    "expected": {
                        "source_commit": "b" * 40,
                        "dirty_state": "clean",
                    },
                    "provenance": {
                        "source_commit": "a" * 40,
                        "dirty_state": "clean",
                    },
                    "failures": ["binary commit does not match expected commit"],
                }
                return RUN_ALL.subprocess.CompletedProcess(
                    command, 1, stdout=json.dumps(report), stderr=""
                )
            if command[:3] == ["git", "rev-parse", "HEAD"]:
                return RUN_ALL.subprocess.CompletedProcess(
                    command, 0, stdout=f"{'b' * 40}\n"
                )
            self.fail(f"unexpected scenario command: {command}")

        with tempfile.TemporaryDirectory() as tmp:
            report_path = Path(tmp) / "report.json"
            with mock.patch.object(RUN_ALL.subprocess, "run", side_effect=fake_run):
                with redirect_stderr(StringIO()):
                    returncode = RUN_ALL.main(
                        [
                            "--codex-bin",
                            "/tmp/codex",
                            "--report-json",
                            str(report_path),
                        ]
                    )
            report = json.loads(report_path.read_text(encoding="utf-8"))

        self.assertEqual(1, returncode)
        self.assertFalse(report["passed"])
        self.assertFalse(report["partial"])
        self.assertEqual(0, report["scenario_count"])
        self.assertEqual("stale", report["provenance"]["status"])
        self.assertFalse(
            any(
                len(command) > 1 and command[1] == str(RUN_ALL.HARNESS)
                for command in commands
            )
        )


if __name__ == "__main__":
    unittest.main()
