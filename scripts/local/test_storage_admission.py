import importlib.util
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


MODULE_PATH = Path(__file__).with_name("storage_admission.py")
SPEC = importlib.util.spec_from_file_location("storage_admission", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {MODULE_PATH}")
storage_admission = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(storage_admission)


class StorageAdmissionTest(unittest.TestCase):
    def test_parse_floor_requires_canonical_positive_decimal(self) -> None:
        for value in ("", "0", "01", "+1", "-1", "1_0", " "):
            with self.subTest(value=value):
                with self.assertRaises(storage_admission.AdmissionError):
                    storage_admission.parse_floor(value, "floor")

        self.assertEqual(
            storage_admission.parse_floor(str((1 << 63) - 1), "floor"),
            (1 << 63) - 1,
        )
        with self.assertRaises(storage_admission.AdmissionError):
            storage_admission.parse_floor(str(1 << 63), "floor")
        with self.assertRaises(storage_admission.AdmissionError):
            storage_admission.parse_floor("1" * 5000, "floor")

    def test_threshold_equality_and_same_filesystem_floors_are_independent(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            target = root / "missing-target"
            usage = SimpleNamespace(f_bavail=10, f_frsize=10)
            with mock.patch.object(
                storage_admission.os, "statvfs", return_value=usage
            ) as statvfs:
                storage_admission.admit(str(root), str(target), "100", "100")

        self.assertEqual(statvfs.call_count, 2)

    def test_capacity_errors_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            with mock.patch.object(
                storage_admission.os,
                "statvfs",
                side_effect=PermissionError("denied"),
            ):
                with self.assertRaises(storage_admission.AdmissionError):
                    storage_admission.admit(
                        temporary_directory, temporary_directory, "1", None
                    )

    def test_symlink_destination_is_canonicalized(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            destination = root / "destination"
            destination.mkdir()
            link = root / "link"
            link.symlink_to(destination, target_is_directory=True)
            storage_admission.admit(str(root), str(link / "new-target"), None, "1")

    def test_symlink_parent_is_resolved_before_dotdot(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            destination = root / "outside" / "volume"
            destination.mkdir(parents=True)
            link = root / "link"
            link.symlink_to(destination, target_is_directory=True)

            resolved = storage_admission.canonical_existing_directory(
                str(link / ".." / "new-target"), "target storage"
            )

        self.assertEqual(resolved, str(destination.parent.resolve()))

    def test_missing_prefix_before_dotdot_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            path = Path(temporary_directory) / "missing" / ".." / "target"
            with self.assertRaises(storage_admission.AdmissionError):
                storage_admission.canonical_existing_directory(str(path), "target")

    def test_symlink_loop_and_non_directory_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            loop = root / "loop"
            loop.symlink_to(loop)
            with self.assertRaises(storage_admission.AdmissionError):
                storage_admission.admit(str(root), str(loop), None, "1")

            file_path = root / "file"
            file_path.write_text("owned\n", encoding="utf-8")
            with self.assertRaises(storage_admission.AdmissionError):
                storage_admission.admit(str(root), str(file_path), None, "1")
            with self.assertRaises(storage_admission.AdmissionError):
                storage_admission.canonical_existing_directory(
                    str(file_path / "child"), "target"
                )

            dangling = root / "dangling"
            dangling.symlink_to(root / "missing")
            with self.assertRaises(storage_admission.AdmissionError):
                storage_admission.canonical_existing_directory(str(dangling), "target")

    def test_intermediate_permission_and_unknown_capacity_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            with mock.patch.object(
                storage_admission.os, "lstat", side_effect=PermissionError("denied")
            ):
                with self.assertRaises(storage_admission.AdmissionError):
                    storage_admission.canonical_existing_directory(
                        str(root / "target"), "target"
                    )

            usage = SimpleNamespace(f_bavail=1, f_frsize=0)
            with mock.patch.object(storage_admission.os, "statvfs", return_value=usage):
                with self.assertRaises(storage_admission.AdmissionError):
                    storage_admission.admit(str(root), str(root), "1", None)


if __name__ == "__main__":
    unittest.main()
