import hashlib
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock


MODULE_PATH = Path(__file__).with_name("feedback_latency.py")
SPEC = importlib.util.spec_from_file_location("feedback_latency", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {MODULE_PATH}")
feedback_latency: Any = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(feedback_latency)


def patch_feedback(name: str, **kwargs: Any) -> Any:
    return mock.patch.object(feedback_latency, name, **kwargs)


class FeedbackLatencyTest(unittest.TestCase):
    def test_controlled_edit_requires_a_nonempty_tracked_diff(self) -> None:
        with self.assertRaises(SystemExit):
            feedback_latency.parse_args(
                [
                    "--lane",
                    "empty-edit",
                    "--scenario",
                    "warm-edit",
                    "--expected-diff-sha256",
                    feedback_latency.EMPTY_DIFF_SHA256,
                    "--",
                    "private-command",
                ]
            )

    def test_uncontrolled_dirty_diff_drift_is_not_stable(self) -> None:
        before = {
            "commit": "a" * 40,
            "dirty": True,
            "untrackedChanges": False,
            "trackedDiffSha256": "1" * 64,
        }
        after = {**before, "trackedDiffSha256": "2" * 64}

        evidence = feedback_latency.source_edit_evidence(before, after, None)

        self.assertFalse(evidence["stable"])
        self.assertFalse(evidence["comparable"])

    def test_controlled_edit_matches_declared_tracked_diff(self) -> None:
        diff = "c" * 64
        source = {
            "commit": "a" * 40,
            "dirty": True,
            "untrackedChanges": False,
            "trackedDiffSha256": diff,
        }
        with tempfile.TemporaryDirectory() as temporary_directory:
            output = Path(temporary_directory) / "evidence.json"
            with (
                patch_feedback("source_identity", return_value=source),
                patch_feedback(
                    "read_sccache_stats", return_value={"status": "unavailable"}
                ),
                mock.patch("subprocess.run", return_value=mock.Mock(returncode=0)),
            ):
                result = feedback_latency.main(
                    [
                        "--lane",
                        "controlled-edit",
                        "--scenario",
                        "warm-edit",
                        "--expected-diff-sha256",
                        diff,
                        "--output",
                        str(output),
                        "--",
                        "private-command",
                    ]
                )
            record = json.loads(output.read_text(encoding="utf-8"))

        self.assertEqual(result, 0)
        self.assertEqual(record["schemaVersion"], 3)
        self.assertEqual(
            record["sourceEdit"],
            {
                "mode": "controlled-edit",
                "expectedDiffSha256": diff,
                "beforeStatus": "matched",
                "afterStatus": "matched",
                "stable": True,
                "comparable": True,
            },
        )
        self.assertTrue(record["comparable"])

    def test_controlled_edit_rejects_wrong_diff_before_command(self) -> None:
        source = {
            "commit": "a" * 40,
            "dirty": True,
            "untrackedChanges": False,
            "trackedDiffSha256": "d" * 64,
        }
        with (
            patch_feedback("source_identity", return_value=source),
            patch_feedback("write_evidence") as write_evidence,
            mock.patch("subprocess.run") as run,
        ):
            result = feedback_latency.main(
                [
                    "--lane",
                    "wrong-edit",
                    "--scenario",
                    "warm-edit",
                    "--expected-diff-sha256",
                    "e" * 64,
                    "--",
                    "private-command",
                ]
            )

        self.assertEqual(result, feedback_latency.HARNESS_EXIT_CODE)
        run.assert_not_called()
        write_evidence.assert_not_called()

    def test_controlled_edit_rejects_untracked_changes(self) -> None:
        source = {
            "commit": "a" * 40,
            "dirty": True,
            "untrackedChanges": True,
            "trackedDiffSha256": "f" * 64,
        }
        with (
            patch_feedback("source_identity", return_value=source),
            patch_feedback("write_evidence") as write_evidence,
        ):
            result = feedback_latency.main(
                [
                    "--lane",
                    "untracked-edit",
                    "--scenario",
                    "warm-edit",
                    "--expected-diff-sha256",
                    "f" * 64,
                    "--",
                    "private-command",
                ]
            )

        self.assertEqual(result, feedback_latency.HARNESS_EXIT_CODE)
        write_evidence.assert_not_called()

    def test_unknown_dirty_checkout_remains_non_comparable(self) -> None:
        source = {
            "commit": "a" * 40,
            "dirty": True,
            "untrackedChanges": False,
            "trackedDiffSha256": "1" * 64,
        }
        with tempfile.TemporaryDirectory() as temporary_directory:
            output = Path(temporary_directory) / "evidence.json"
            with (
                patch_feedback("source_identity", return_value=source),
                patch_feedback(
                    "read_sccache_stats", return_value={"status": "unavailable"}
                ),
                mock.patch("subprocess.run", return_value=mock.Mock(returncode=0)),
            ):
                result = feedback_latency.main(
                    [
                        "--lane",
                        "unknown-dirty",
                        "--scenario",
                        "warm-edit",
                        "--allow-dirty",
                        "--output",
                        str(output),
                        "--",
                        "private-command",
                    ]
                )
            record = json.loads(output.read_text(encoding="utf-8"))

        self.assertEqual(result, 0)
        self.assertEqual(record["sourceEdit"]["beforeStatus"], "unknown-dirty")
        self.assertFalse(record["sourceEdit"]["comparable"])
        self.assertFalse(record["comparable"])

    def test_controlled_edit_drift_after_command_is_non_comparable(self) -> None:
        diff = "2" * 64
        before = {
            "commit": "a" * 40,
            "dirty": True,
            "untrackedChanges": False,
            "trackedDiffSha256": diff,
        }
        after = {**before, "trackedDiffSha256": "3" * 64}
        with tempfile.TemporaryDirectory() as temporary_directory:
            output = Path(temporary_directory) / "evidence.json"
            with (
                patch_feedback("source_identity", side_effect=[before, after]),
                patch_feedback(
                    "read_sccache_stats", return_value={"status": "unavailable"}
                ),
                mock.patch("subprocess.run", return_value=mock.Mock(returncode=0)),
            ):
                result = feedback_latency.main(
                    [
                        "--lane",
                        "drifted-edit",
                        "--scenario",
                        "warm-edit",
                        "--expected-diff-sha256",
                        diff,
                        "--output",
                        str(output),
                        "--",
                        "private-command",
                    ]
                )
            record = json.loads(output.read_text(encoding="utf-8"))

        self.assertEqual(result, 0)
        self.assertEqual(record["sourceEdit"]["afterStatus"], "diff-mismatch")
        self.assertFalse(record["sourceEdit"]["stable"])
        self.assertFalse(record["comparable"])

    def test_machine_identity_is_bounded_and_hides_private_inputs(self) -> None:
        environment = {
            "GITHUB_ACTIONS": "true",
            "RUNNER_ENVIRONMENT": "github-hosted",
            "RUNNER_NAME": "private-runner-alice",
            "RUNNER_OS": "macOS",
            "RUNNER_ARCH": "ARM64",
            "ImageOS": "macos26",
        }

        identity = feedback_latency.machine_identity(environment)

        self.assertEqual(
            identity,
            {
                "kind": "github-hosted",
                "os": "macOS",
                "arch": "ARM64",
                "cpuCount": os.cpu_count(),
                "machineId": identity["machineId"],
                "image": "macos26",
            },
        )
        self.assertRegex(identity["machineId"], r"^machine-[0-9a-f]{12}$")
        self.assertNotIn("alice", json.dumps(identity))

        self_hosted = feedback_latency.machine_identity(
            {
                "GITHUB_ACTIONS": "true",
                "RUNNER_ENVIRONMENT": "self-hosted",
                "RUNNER_NAME": "private-runner-alice",
            }
        )
        self.assertEqual(self_hosted["kind"], "self-hosted")
        self.assertEqual(self_hosted["machineId"], "local-machine")
        self.assertNotIn("alice", json.dumps(self_hosted))

    def test_parse_sccache_stats_normalizes_nested_counters(self) -> None:
        output = json.dumps(
            {
                "stats": {
                    "compile_requests": 12,
                    "requests_executed": 10,
                    "cache_hits": {"counts": {"Rust": 7}, "adv_counts": {}},
                    "cache_misses": {"counts": {"Rust": 3}, "adv_counts": {}},
                    "cache_writes": 3,
                    "cache_write_errors": 1,
                    "cache_errors": {"counts": {"Rust": 2}, "adv_counts": {}},
                },
                "cache_size": 1024,
                "max_cache_size": 4096,
            }
        )

        self.assertEqual(
            feedback_latency.parse_sccache_stats(output),
            {
                "compileRequests": 12,
                "requestsExecuted": 10,
                "cacheHits": 7,
                "cacheMisses": 3,
                "cacheWrites": 3,
                "cacheWriteErrors": 1,
                "cacheErrors": 2,
                "nonCacheableRequests": None,
                "cacheSizeBytes": 1024,
                "maxCacheSizeBytes": 4096,
            },
        )

    def test_lane_name_rejects_paths_and_private_text(self) -> None:
        with self.assertRaises(SystemExit):
            feedback_latency.parse_args(
                [
                    "--lane",
                    "/Users/alice/private-lane",
                    "--scenario",
                    "warm-noop",
                    "--",
                    "true",
                ]
            )

    def test_sccache_delta_reports_hit_rate_and_counter_reset(self) -> None:
        before: dict[str, Any] = {
            "status": "available",
            "metrics": {
                "compileRequests": 10,
                "cacheHits": 4,
                "cacheMisses": 2,
                "cacheSizeBytes": 100,
                "maxCacheSizeBytes": 1000,
            },
        }
        after: dict[str, Any] = {
            "status": "available",
            "metrics": {
                "compileRequests": 20,
                "cacheHits": 10,
                "cacheMisses": 4,
                "cacheSizeBytes": 200,
                "maxCacheSizeBytes": 1000,
            },
        }

        self.assertEqual(
            feedback_latency.sccache_delta(before, after),
            {
                "status": "available",
                "reason": None,
                "delta": {
                    "compileRequests": 10,
                    "cacheHits": 6,
                    "cacheMisses": 2,
                    "hitRatePercent": 75.0,
                },
                "gauges": {"cacheSizeBytes": 200, "maxCacheSizeBytes": 1000},
                "serverIdentity": {
                    "before": {"status": "unknown", "fingerprint": None},
                    "after": {"status": "unknown", "fingerprint": None},
                },
            },
        )
        after["metrics"]["compileRequests"] = 1
        self.assertEqual(
            feedback_latency.sccache_delta(before, after),
            {
                "status": "counter-reset",
                "reason": "sccache counters decreased during the command",
                "delta": None,
                "gauges": {"cacheSizeBytes": 200, "maxCacheSizeBytes": 1000},
                "serverIdentity": {
                    "before": {"status": "unknown", "fingerprint": None},
                    "after": {"status": "unknown", "fingerprint": None},
                },
            },
        )

    def test_sccache_endpoint_drift_is_opaque_and_non_comparable(self) -> None:
        before_output = json.dumps(
            {"cache_location": "/private/alice/cache", "version": "v1"}
        )
        after_output = json.dumps(
            {"cache_location": "/private/bob/cache", "version": "v1"}
        )
        before_identity = feedback_latency.sccache_server_identity(before_output)
        after_identity = feedback_latency.sccache_server_identity(after_output)
        result = feedback_latency.sccache_delta(
            {
                "status": "available",
                "metrics": {"cacheHits": 1, "cacheMisses": 0},
                "serverIdentity": before_identity,
            },
            {
                "status": "available",
                "metrics": {"cacheHits": 2, "cacheMisses": 0},
                "serverIdentity": after_identity,
            },
        )

        self.assertEqual(result["status"], "server-changed")
        self.assertEqual(result["serverIdentity"]["before"]["status"], "known")
        self.assertNotIn("/private", json.dumps(result))
        self.assertNotEqual(
            result["serverIdentity"]["before"]["fingerprint"],
            result["serverIdentity"]["after"]["fingerprint"],
        )

    def test_sccache_unknown_identity_is_explicit(self) -> None:
        result = feedback_latency.sccache_delta(
            {"status": "available", "metrics": {}},
            {"status": "available", "metrics": {}},
        )

        self.assertEqual(
            result["serverIdentity"],
            {
                "before": {"status": "unknown", "fingerprint": None},
                "after": {"status": "unknown", "fingerprint": None},
            },
        )

    def test_server_change_makes_evidence_non_comparable(self) -> None:
        source = {"commit": "a" * 40, "dirty": False}
        before = feedback_latency.sccache_server_identity(
            json.dumps({"cache_location": "/private/a", "version": "v1"})
        )
        after = feedback_latency.sccache_server_identity(
            json.dumps({"cache_location": "/private/b", "version": "v1"})
        )
        cache_before = {
            "status": "available",
            "metrics": {"cacheHits": 1, "cacheMisses": 0},
            "serverIdentity": before,
        }
        cache_after = {
            "status": "available",
            "metrics": {"cacheHits": 2, "cacheMisses": 0},
            "serverIdentity": after,
        }
        with tempfile.TemporaryDirectory() as temporary_directory:
            output = Path(temporary_directory) / "evidence.json"
            with (
                patch_feedback("source_identity", return_value=source),
                patch_feedback(
                    "read_sccache_stats", side_effect=[cache_before, cache_after]
                ),
                mock.patch("subprocess.run", return_value=mock.Mock(returncode=0)),
            ):
                result = feedback_latency.main(
                    [
                        "--lane",
                        "server-change",
                        "--scenario",
                        "warm-noop",
                        "--output",
                        str(output),
                        "--",
                        "private-command",
                    ]
                )
            record = json.loads(output.read_text(encoding="utf-8"))

        self.assertEqual(result, 0)
        self.assertEqual(record["sccache"]["status"], "server-changed")
        self.assertFalse(record["comparable"])
        self.assertNotIn("/private", json.dumps(record))

    def test_rusty_v8_preflight_verifies_manifest_checksums(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            manifest_dir = root / "third_party" / "v8"
            manifest_dir.mkdir(parents=True)
            archive = root / "librusty_v8_test.a.gz"
            binding = root / "src_binding_test.rs"
            archive.write_bytes(b"archive")
            binding.write_bytes(b"binding")
            manifest = manifest_dir / "rusty_v8_test_release.sha256"
            manifest.write_text(
                "\n".join(
                    (
                        f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}",
                        f"{hashlib.sha256(binding.read_bytes()).hexdigest()}  {binding.name}",
                    )
                ),
                encoding="utf-8",
            )
            environment = {
                "RUSTY_V8_ARCHIVE": str(archive),
                "RUSTY_V8_SRC_BINDING_PATH": str(binding),
            }

            self.assertEqual(
                feedback_latency.trusted_rusty_v8_preflight(root, environment),
                {
                    "status": "ready",
                    "checked": ["RUSTY_V8_ARCHIVE", "RUSTY_V8_SRC_BINDING_PATH"],
                    "failures": [],
                },
            )
            archive.write_bytes(b"tampered")
            self.assertEqual(
                feedback_latency.trusted_rusty_v8_preflight(root, environment),
                {
                    "status": "failed",
                    "checked": ["RUSTY_V8_ARCHIVE", "RUSTY_V8_SRC_BINDING_PATH"],
                    "failures": [
                        "RUSTY_V8_ARCHIVE checksum does not match the trusted manifest"
                    ],
                },
            )

    def test_helper_preflight_never_records_paths(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            helper = Path(temporary_directory) / "private-helper"
            helper.write_text("helper", encoding="utf-8")
            helper.chmod(0o755)

            result = feedback_latency.helper_preflight(
                [str(helper), str(helper.with_name("missing-private-helper"))]
            )

        self.assertEqual(
            result,
            {
                "status": "failed",
                "requiredCount": 2,
                "failures": ["required helper is missing"],
            },
        )
        self.assertNotIn(temporary_directory, json.dumps(result))

    def test_main_preserves_command_failure_and_writes_allowlisted_evidence(
        self,
    ) -> None:
        source = {"commit": "a" * 40, "dirty": False}
        cache = {"status": "unavailable", "reason": "not-installed"}
        process = mock.Mock(returncode=23)
        with tempfile.TemporaryDirectory() as temporary_directory:
            output = Path(temporary_directory) / "evidence.json"
            with (
                patch_feedback("source_identity", return_value=source),
                patch_feedback("machine_identity") as identity,
                patch_feedback(
                    "build_context", side_effect=OSError("unavailable inputs")
                ),
                patch_feedback("read_sccache_stats", return_value=cache),
                mock.patch("subprocess.run", return_value=process) as run,
            ):
                identity.return_value = {
                    "kind": "local",
                    "os": "Darwin",
                    "arch": "arm64",
                    "cpuCount": 12,
                    "machineId": "machine-0123456789ab",
                }
                exit_code = feedback_latency.main(
                    [
                        "--lane",
                        "focused-core",
                        "--scenario",
                        "warm-noop",
                        "--output",
                        str(output),
                        "--",
                        "private-command",
                        "/Users/alice/private-path",
                    ]
                )
            evidence = json.loads(output.read_text(encoding="utf-8"))

        run.assert_called_once_with(["private-command", "/Users/alice/private-path"])
        self.assertEqual(exit_code, 23)
        self.assertEqual(evidence["exitCode"], 23)
        self.assertEqual(evidence["commandStatus"], "completed")
        self.assertEqual(
            set(evidence["phaseDurationsMs"]), {"preflight", "command", "telemetry"}
        )
        serialized = json.dumps(evidence)
        summary = feedback_latency.render_summary(evidence)
        self.assertNotIn("private-command", serialized)
        self.assertNotIn("/Users/alice", serialized)
        self.assertIn("### Rust feedback latency", summary)

    def test_storage_sampling_preserves_real_child_exit_and_bounds_evidence(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            output = Path(temporary_directory) / "evidence.json"
            paths = []
            for index in range(8):
                paths.extend(["--storage-path", f"role{index}={temporary_directory}"])
            with (
                patch_feedback(
                    "source_identity", return_value={"commit": "a" * 40, "dirty": False}
                ),
                patch_feedback(
                    "read_sccache_stats", return_value={"status": "unavailable"}
                ),
            ):
                result = feedback_latency.main(
                    [
                        "--lane",
                        "storage-child",
                        "--scenario",
                        "warm-noop",
                        "--configuration",
                        "dev-default",
                        "--concurrent-builds",
                        "--output",
                        str(output),
                        *paths,
                        "--",
                        sys.executable,
                        "-c",
                        "raise SystemExit(7)",
                    ]
                )
            record = json.loads(output.read_text())
            self.assertLess(output.stat().st_size, feedback_latency.MAX_JSON_BYTES)
        self.assertEqual(result, 7)
        self.assertFalse(record["comparable"])
        self.assertEqual(record["storage"]["status"], "available")
        self.assertGreaterEqual(record["storage"]["sampleCount"], 2)
        self.assertEqual(len(record["storage"]["filesystems"]), 1)
        self.assertNotIn(temporary_directory, json.dumps(record))

    def test_eight_distinct_filesystems_fit_the_evidence_budget(self) -> None:
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as temporary_directory:
            output = Path(temporary_directory) / "evidence.json"
            roles = [str(index) + "a" * 63 for index in range(8)]
            storage = {
                "paths": {
                    role: {
                        "status": "available",
                        "filesystemId": "filesystem-" + str(index) * 16,
                    }
                    for index, role in enumerate(roles)
                },
                "filesystems": {
                    "filesystem-" + str(index) * 16: {
                        "status": "available",
                        "totalBytes": 10**15,
                        "observedFreeBytes": 10**14,
                    }
                    for index in range(8)
                },
            }
            args = [
                "--lane",
                "bounded-evidence",
                "--scenario",
                "warm-noop",
                "--output",
                str(output),
            ]
            for role in roles:
                args.extend(["--storage-path", f"{role}={temporary_directory}"])
            with (
                patch_feedback(
                    "source_identity", return_value={"commit": "a" * 40, "dirty": False}
                ),
                patch_feedback(
                    "read_sccache_stats", return_value={"status": "unavailable"}
                ),
                patch("feedback_storage.storage_snapshot", return_value=storage),
            ):
                self.assertEqual(
                    feedback_latency.main([*args, "--", sys.executable, "-c", "pass"]),
                    0,
                )
            self.assertLess(output.stat().st_size, feedback_latency.MAX_JSON_BYTES)
            self.assertEqual(
                len(json.loads(output.read_text())["storage"]["filesystems"]), 8
            )

    def test_dirty_checkout_requires_explicit_acknowledgement(self) -> None:
        with (
            patch_feedback(
                "source_identity", return_value={"commit": "b" * 40, "dirty": True}
            ),
            patch_feedback("write_evidence") as write_evidence,
        ):
            exit_code = feedback_latency.main(
                ["--lane", "local-edit", "--scenario", "warm-edit", "--", "true"]
            )

        self.assertEqual(exit_code, feedback_latency.HARNESS_EXIT_CODE)
        write_evidence.assert_not_called()


if __name__ == "__main__":
    unittest.main()
