#!/usr/bin/env python3

import argparse
import os
import shutil
import stat
import subprocess
import tempfile
from collections.abc import Callable, Sequence
from pathlib import Path


FILE_TOOL = "/usr/bin/file"
LIPO_TOOL = "/usr/bin/lipo"
ARCHIVE_SUFFIXES = frozenset({".a", ".dylib", ".lib", ".rlib", ".so"})
EXPECTED_FILENAMES = ("codex", "codex-code-mode-host")


class StagingError(RuntimeError):
    pass


CommandRunner = Callable[[Sequence[str]], str]
PathReplacer = Callable[[Path, Path], None]


def run_command(arguments: Sequence[str]) -> str:
    result = subprocess.run(
        arguments,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "no diagnostic"
        raise StagingError(
            f"executable inspection failed ({' '.join(arguments)}): {detail}"
        )
    return result.stdout.strip()


def validate_macos_executable(
    path: Path,
    expected_architecture: str,
    *,
    command_runner: CommandRunner = run_command,
) -> tuple[str, tuple[str, ...]]:
    try:
        mode = path.stat().st_mode
    except OSError as error:
        raise StagingError(f"missing executable candidate: {path}") from error

    if not stat.S_ISREG(mode):
        raise StagingError(f"executable candidate is not a regular file: {path}")
    if path.suffix.lower() in ARCHIVE_SUFFIXES:
        raise StagingError(f"archive or library output cannot be staged: {path}")
    if not os.access(path, os.X_OK):
        raise StagingError(f"executable candidate is not executable: {path}")

    file_description = command_runner((FILE_TOOL, "-b", "--", str(path)))
    lowered_description = file_description.lower()
    if any(
        rejected in lowered_description
        for rejected in ("archive", "directory", "library")
    ):
        raise StagingError(
            f"archive, library, or directory output cannot be staged: "
            f"{path} ({file_description})"
        )
    if "mach-o 64-bit executable" not in lowered_description:
        raise StagingError(
            f"expected a Mach-O 64-bit executable: {path} ({file_description})"
        )

    architectures = tuple(command_runner((LIPO_TOOL, "-archs", str(path))).split())
    if expected_architecture not in architectures:
        found = ", ".join(architectures) or "none"
        raise StagingError(
            f"expected architecture {expected_architecture} in {path}; found {found}"
        )
    return file_description, architectures


def stage_pair(
    codex_source: Path,
    host_source: Path,
    destination: Path,
    expected_architecture: str,
    *,
    command_runner: CommandRunner = run_command,
    path_replacer: PathReplacer = os.replace,
) -> tuple[Path, Path]:
    sources = (codex_source, host_source)
    for source in sources:
        validate_macos_executable(
            source,
            expected_architecture,
            command_runner=command_runner,
        )

    try:
        destination.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        raise StagingError(
            f"cannot prepare staging destination: {destination}: {error}"
        ) from error
    if not destination.is_dir():
        raise StagingError(f"staging destination is not a directory: {destination}")

    prepared: list[tuple[Path, Path]] = []
    temporary_paths: list[Path] = []
    try:
        for source, filename in zip(sources, EXPECTED_FILENAMES, strict=True):
            descriptor, temporary_name = tempfile.mkstemp(
                prefix=f".{filename}.",
                dir=destination,
            )
            os.close(descriptor)
            temporary_path = Path(temporary_name)
            temporary_paths.append(temporary_path)
            shutil.copyfile(source, temporary_path)
            temporary_path.chmod(0o755)
            validate_macos_executable(
                temporary_path,
                expected_architecture,
                command_runner=command_runner,
            )
            prepared.append((temporary_path, destination / filename))

        staged_paths = tuple(staged_path for _, staged_path in prepared)
        try:
            for temporary_path, staged_path in prepared:
                path_replacer(temporary_path, staged_path)

            for staged_path in staged_paths:
                validate_macos_executable(
                    staged_path,
                    expected_architecture,
                    command_runner=command_runner,
                )
            if tuple(path.name for path in staged_paths) != EXPECTED_FILENAMES:
                raise StagingError(
                    "staged executable filenames do not match the SDK contract"
                )
            if len({path.parent.resolve() for path in staged_paths}) != 1:
                raise StagingError("staged executables are not siblings")
        except (OSError, StagingError) as error:
            for staged_path in staged_paths:
                staged_path.unlink(missing_ok=True)
            if isinstance(error, StagingError):
                raise
            raise StagingError(
                f"failed to publish staged executable pair: {error}"
            ) from error
        return staged_paths
    finally:
        for temporary_path in temporary_paths:
            temporary_path.unlink(missing_ok=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Fail-closed staging for the Bazel-built SDK executable pair."
    )
    parser.add_argument("--codex-source", type=Path, required=True)
    parser.add_argument("--host-source", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--expected-architecture", required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    try:
        staged_paths = stage_pair(
            args.codex_source,
            args.host_source,
            args.destination,
            args.expected_architecture,
        )
        for staged_path in staged_paths:
            description, architectures = validate_macos_executable(
                staged_path,
                args.expected_architecture,
            )
            print(
                f"staged {staged_path.name}: {description}; "
                f"architectures={','.join(architectures)}"
            )
    except StagingError as error:
        raise SystemExit(f"SDK binary staging failed: {error}") from error


if __name__ == "__main__":
    main()
