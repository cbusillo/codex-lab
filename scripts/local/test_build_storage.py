import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


MODULE_PATH = Path(__file__).with_name("build_storage.py")
SPEC = importlib.util.spec_from_file_location("build_storage", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {MODULE_PATH}")
build_storage = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build_storage)


class BuildStorageTest(unittest.TestCase):
    def test_storage_snapshot_is_public_safe_and_distinguishes_statuses(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            present = root / "cargo-target"
            present.mkdir()
            sibling = present / "debug"
            sibling.mkdir()
            missing = root / "missing-cache"
            paths = {"cargoTarget": present, "cargoDebug": sibling, "missing": missing}
            usage = SimpleNamespace(free=123, total=456)
            with mock.patch.object(
                build_storage.shutil, "disk_usage", return_value=usage
            ) as disk_usage:
                report = build_storage.storage_snapshot(paths)

        self.assertEqual(report["schemaVersion"], 1)
        filesystem_id = report["paths"]["cargoTarget"]["filesystemId"]
        self.assertEqual(
            report["filesystems"][filesystem_id],
            {
                "status": "available",
                "observedFreeBytes": 123,
                "totalBytes": 456,
            },
        )
        self.assertEqual(report["paths"]["cargoDebug"]["filesystemId"], filesystem_id)
        disk_usage.assert_called_once()
        self.assertEqual(report["paths"]["missing"], {"status": "missing"})
        self.assertNotIn(temporary_directory, json.dumps(report))

    def test_storage_snapshot_does_not_turn_disk_usage_failure_into_zero(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            path = Path(temporary_directory)
            with mock.patch.object(
                build_storage.shutil,
                "disk_usage",
                side_effect=OSError("permission denied"),
            ):
                report = build_storage.storage_snapshot({"cache": path})

        self.assertEqual(report["paths"]["cache"]["status"], "unavailable")
        self.assertIn("filesystemId", report["paths"]["cache"])

    def test_allocated_snapshot_deduplicates_parent_and_child(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            parent = root / "target"
            child = parent / "debug"
            child.mkdir(parents=True)
            paths = {"parent": parent, "child": child, "same": parent}
            with (
                mock.patch.object(build_storage.shutil, "which", return_value="du"),
                mock.patch.object(
                    build_storage.subprocess,
                    "run",
                    return_value=SimpleNamespace(returncode=0, stdout="7\tignored\n"),
                ) as run,
            ):
                report = build_storage.allocated_snapshot(paths)

        self.assertEqual(report["status"], "available")
        self.assertEqual(
            report["paths"]["parent"], {"status": "available", "allocatedBytes": 7168}
        )
        self.assertEqual(
            report["paths"]["child"],
            {"status": "overlap", "allocatedBytes": None, "measuredAs": "parent"},
        )
        self.assertEqual(
            report["paths"]["same"],
            {"status": "overlap", "allocatedBytes": None, "measuredAs": "parent"},
        )
        self.assertEqual(report["totalAllocatedBytes"], 7168)
        run.assert_called_once()
        self.assertEqual(run.call_args.kwargs["timeout"], 5.0)
        self.assertNotIn(temporary_directory, json.dumps(report))

    def test_nested_different_filesystems_are_measured_separately(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            parent = Path(temporary_directory)
            child = parent / "mounted"
            child.mkdir()
            with (
                mock.patch.object(
                    build_storage,
                    "_filesystem_id",
                    side_effect=lambda path: (
                        "child-device" if path == child.resolve() else "parent-device"
                    ),
                ),
                mock.patch.object(
                    build_storage, "_du_bytes", side_effect=[100, 200]
                ) as measure,
            ):
                report = build_storage.allocated_snapshot(
                    {"parent": parent, "child": child}
                )
        self.assertEqual(measure.call_count, 2)
        self.assertEqual(report["status"], "available")
        self.assertEqual(report["totalAllocatedBytes"], 300)

    def test_allocated_snapshot_reports_missing_and_unavailable_du(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            path = Path(temporary_directory)
            missing = path / "missing"
            with mock.patch.object(build_storage.shutil, "which", return_value=None):
                report = build_storage.allocated_snapshot(
                    {"present": path, "missing": missing}
                )

        self.assertEqual(report["status"], "unavailable")
        self.assertEqual(
            report["paths"]["present"],
            {"status": "unavailable", "allocatedBytes": None},
        )
        self.assertEqual(
            report["paths"]["missing"],
            {"status": "missing", "allocatedBytes": None},
        )
        self.assertNotIn("totalAllocatedBytes", report)

    def test_allocated_snapshot_distinguishes_timeout_from_other_failures(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            path = Path(temporary_directory)
            timeout = build_storage.subprocess.TimeoutExpired("du", 5.0)
            with (
                mock.patch.object(build_storage.shutil, "which", return_value="du"),
                mock.patch.object(build_storage.subprocess, "run", side_effect=timeout),
            ):
                report = build_storage.allocated_snapshot({"cache": path})

        self.assertEqual(report["status"], "unavailable")
        self.assertEqual(
            report["paths"]["cache"],
            {"status": "timeout", "allocatedBytes": None},
        )

    def test_cli_requires_explicit_paths_and_emits_optional_allocation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            path = Path(temporary_directory)
            with (
                mock.patch.object(
                    build_storage.shutil,
                    "disk_usage",
                    return_value=SimpleNamespace(free=1, total=2),
                ),
                mock.patch.object(build_storage.shutil, "which", return_value=None),
                mock.patch("builtins.print") as output,
            ):
                self.assertEqual(
                    build_storage.main(["--path", f"cache={path}", "--allocated"]),
                    0,
                )
        payload = json.loads(output.call_args.args[0])
        filesystem_id = payload["paths"]["cache"]["filesystemId"]
        self.assertEqual(payload["filesystems"][filesystem_id]["observedFreeBytes"], 1)
        self.assertEqual(payload["allocation"]["status"], "unavailable")

    def test_bounds_reject_unbounded_requests(self) -> None:
        paths = {
            f"cache{index}": Path(f"/tmp/cache{index}")
            for index in range(build_storage.MAX_PATH_COUNT + 1)
        }
        with self.assertRaises(build_storage.StorageError):
            build_storage.storage_snapshot(paths)
        with self.assertRaises(build_storage.StorageError):
            build_storage.allocated_snapshot(
                {}, timeout_seconds=build_storage.MAX_ALLOCATION_TIMEOUT_SECONDS + 1
            )


if __name__ == "__main__":
    unittest.main()
