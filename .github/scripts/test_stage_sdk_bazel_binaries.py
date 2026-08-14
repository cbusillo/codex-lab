import tempfile
import unittest
import os
from pathlib import Path

from stage_sdk_bazel_binaries import FILE_TOOL
from stage_sdk_bazel_binaries import LIPO_TOOL
from stage_sdk_bazel_binaries import StagingError
from stage_sdk_bazel_binaries import stage_pair
from stage_sdk_bazel_binaries import validate_macos_executable


def arm64_macho(arguments: tuple[str, ...]) -> str:
    if arguments[0] == FILE_TOOL:
        return "Mach-O 64-bit executable arm64"
    if arguments[0] == LIPO_TOOL:
        return "arm64"
    raise AssertionError(f"unexpected command: {arguments}")


class StageSdkBazelBinariesTest(unittest.TestCase):
    def executable_file(self, directory: Path, name: str = "binary") -> Path:
        path = directory / name
        path.write_bytes(b"test executable")
        path.chmod(0o755)
        return path

    def test_stages_regular_executables_as_exact_siblings(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex = self.executable_file(root, "bazel-codex")
            host = self.executable_file(root, "bazel-host")
            destination = root / "stage"

            staged = stage_pair(
                codex,
                host,
                destination,
                "arm64",
                command_runner=arm64_macho,
            )

            self.assertEqual(
                staged,
                (destination / "codex", destination / "codex-code-mode-host"),
            )
            self.assertEqual(
                [path.read_bytes() for path in staged],
                [codex.read_bytes(), host.read_bytes()],
            )
            self.assertTrue(
                all(path.is_file() and path.stat().st_mode & 0o111 for path in staged)
            )

    def test_rejects_the_library_target_even_if_it_is_executable(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            library = self.executable_file(Path(temp_dir), "libhost.rlib")

            with self.assertRaisesRegex(StagingError, "archive or library output"):
                validate_macos_executable(
                    library,
                    "arm64",
                    command_runner=arm64_macho,
                )

    def test_rejects_a_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            directory = Path(temp_dir)

            with self.assertRaisesRegex(StagingError, "not a regular file"):
                validate_macos_executable(
                    directory,
                    "arm64",
                    command_runner=arm64_macho,
                )

    def test_rejects_a_non_executable_file(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "host"
            path.write_bytes(b"not executable")
            path.chmod(0o644)

            with self.assertRaisesRegex(StagingError, "is not executable"):
                validate_macos_executable(
                    path,
                    "arm64",
                    command_runner=arm64_macho,
                )

    def test_rejects_an_archive_without_an_archive_suffix(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = self.executable_file(Path(temp_dir), "host")

            def archive(arguments: tuple[str, ...]) -> str:
                if arguments[0] == FILE_TOOL:
                    return "current ar archive random library"
                return "arm64"

            with self.assertRaisesRegex(StagingError, "archive, library, or directory"):
                validate_macos_executable(
                    path,
                    "arm64",
                    command_runner=archive,
                )

    def test_rejects_the_wrong_architecture(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = self.executable_file(Path(temp_dir), "host")

            def x86_64_macho(arguments: tuple[str, ...]) -> str:
                if arguments[0] == FILE_TOOL:
                    return "Mach-O 64-bit executable x86_64"
                return "x86_64"

            with self.assertRaisesRegex(StagingError, "expected architecture arm64"):
                validate_macos_executable(
                    path,
                    "arm64",
                    command_runner=x86_64_macho,
                )

    def test_rejects_a_file_as_the_staging_destination(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex = self.executable_file(root, "bazel-codex")
            host = self.executable_file(root, "bazel-host")
            destination = self.executable_file(root, "stage")

            with self.assertRaisesRegex(
                StagingError, "cannot prepare staging destination"
            ):
                stage_pair(
                    codex,
                    host,
                    destination,
                    "arm64",
                    command_runner=arm64_macho,
                )

    def test_restores_existing_pair_when_publication_fails_partway(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex = self.executable_file(root, "bazel-codex")
            host = self.executable_file(root, "bazel-host")
            destination = root / "stage"
            destination.mkdir()
            existing_codex = self.executable_file(destination, "codex")
            existing_host = self.executable_file(destination, "codex-code-mode-host")
            existing_codex.write_bytes(b"existing codex")
            existing_host.write_bytes(b"existing host")
            replacement_count = 0

            def fail_second_replacement(source: Path, target: Path) -> None:
                nonlocal replacement_count
                replacement_count += 1
                if replacement_count == 2:
                    raise OSError("simulated publication failure")
                os.replace(source, target)

            with self.assertRaisesRegex(
                StagingError, "failed to publish staged executable pair"
            ):
                stage_pair(
                    codex,
                    host,
                    destination,
                    "arm64",
                    command_runner=arm64_macho,
                    path_replacer=fail_second_replacement,
                )

            self.assertEqual(existing_codex.read_bytes(), b"existing codex")
            self.assertEqual(existing_host.read_bytes(), b"existing host")


if __name__ == "__main__":
    unittest.main()
