import json
import os
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from feedback_context import build_context
from feedback_storage import StorageSampler


def snapshot(free, *, filesystem="a"):
    return {
        "paths": {"target": {"status": "available", "filesystemId": filesystem}},
        "filesystems": {
            filesystem: {
                "status": "available",
                "observedFreeBytes": free,
                "totalBytes": 1000,
            }
        },
    }


class FeedbackStorageTest(unittest.TestCase):
    def test_aggregates_low_water_and_marks_mount_change_degraded(self):
        sampler = StorageSampler({"target": Path("/private")})
        with mock.patch(
            "feedback_storage.storage_snapshot",
            side_effect=[
                snapshot(900),
                snapshot(600),
                snapshot(800),
                snapshot(999, filesystem="b"),
            ],
        ):
            for _ in range(4):
                sampler.sample()
        self.assertEqual(sampler.result["status"], "degraded")
        self.assertEqual(sampler.result["attribution"], "shared-host")
        self.assertEqual(
            sampler.result["filesystems"],
            {
                "a": {
                    "totalBytes": 1000,
                    "observedFreeBytesFirst": 900,
                    "observedFreeBytesMin": 600,
                    "observedFreeBytesLast": 800,
                    "sampleCount": 3,
                }
            },
        )
        self.assertNotIn("/private", json.dumps(sampler.result))

    def test_sampler_failure_degrades_and_background_thread_stops(self):
        sampler = StorageSampler({"target": Path("/private")}, interval_seconds=0.01)
        with mock.patch(
            "feedback_storage.storage_snapshot", side_effect=OSError("private path")
        ):
            sampler.start()
            result = sampler.finish()
        self.assertFalse(sampler.thread.is_alive())
        self.assertEqual(result["status"], "degraded")
        self.assertNotIn("private path", json.dumps(result))

    def test_stalled_filesystem_sampling_has_bounded_start_and_finish(self):
        blocked = threading.Event()
        sampler = StorageSampler({"target": Path("/stalled")})
        with mock.patch(
            "feedback_storage.storage_snapshot", side_effect=lambda _: blocked.wait(10)
        ):
            started = time.monotonic()
            try:
                sampler.start()
                result = sampler.finish()
                self.assertLess(time.monotonic() - started, 4)
                self.assertEqual(
                    result,
                    {
                        "status": "degraded",
                        "attribution": "shared-host",
                        "reason": "sampler-timeout",
                        "sampleIntervalMs": 1000,
                        "sampleCount": 0,
                        "paths": {},
                        "filesystems": {},
                    },
                )
            finally:
                blocked.set()
                sampler.thread.join(timeout=1)

    def test_thread_start_failure_remains_optional_telemetry(self):
        sampler = StorageSampler({"target": Path("/private")})
        with mock.patch.object(
            sampler.thread, "start", side_effect=RuntimeError("thread exhaustion")
        ):
            sampler.start()
            result = sampler.finish()
        self.assertEqual(result["status"], "degraded")
        self.assertEqual(result["reason"], "sampler-start-failed")
        self.assertEqual(result["sampleCount"], 0)

    def test_context_fingerprints_change_without_exposing_flags(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "codex-rs").mkdir()
            (root / "codex-rs/Cargo.lock").write_text("lockfile")
            with mock.patch.dict(
                os.environ,
                {"RUSTFLAGS": "--remap-path-prefix=/private/source=src"},
                clear=True,
            ):
                first = build_context(root, ["cargo", "check"], "dev-default")
            with mock.patch.dict(
                os.environ, {"RUSTFLAGS": "--cfg changed"}, clear=True
            ):
                second = build_context(root, ["cargo", "check"], "dev-default")
        self.assertEqual(first["environmentKeys"], ["RUSTFLAGS"])
        self.assertNotEqual(
            first["environmentFingerprint"], second["environmentFingerprint"]
        )
        self.assertEqual(first["fileFingerprints"]["toolchain"], None)
        self.assertNotIn("/private", json.dumps(first))
        self.assertNotIn(temporary, json.dumps(first))


if __name__ == "__main__":
    unittest.main()
