#!/usr/bin/env python3
"""Collect a bounded, public-safe snapshot of build storage.

The normal snapshot only asks the operating system for filesystem capacity. It
does not walk a cache or estimate reclaimable space. Recursive allocation
measurement is an explicit CLI option for a small, caller-supplied set of
classified paths.
"""

import argparse
import hashlib
import json
import math
import os
import platform
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = 1
MAX_PATH_COUNT = 8
MAX_NAME_LENGTH = 64
MAX_ALLOCATION_TIMEOUT_SECONDS = 30.0
DEFAULT_ALLOCATION_TIMEOUT_SECONDS = 5.0
VOLUME_ID_SALT = b"codex-lab-build-storage-volume-v1\0"
LOGICAL_NAME_CHARS = frozenset(
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-"
)


class StorageError(ValueError):
    """Raised when an inventory request is outside its bounded contract."""


class AllocationMeasurementError(OSError):
    """An allocation measurement failed with a public-safe status."""

    def __init__(self, status: str):
        super().__init__(status)
        self.status = status


def _validate_name(name: str) -> str:
    if (
        not isinstance(name, str)
        or not name
        or len(name) > MAX_NAME_LENGTH
        or name[0] not in LOGICAL_NAME_CHARS - {".", "-", "_"}
        or any(character not in LOGICAL_NAME_CHARS for character in name)
    ):
        raise StorageError(
            "logical path names must start with an alphanumeric character and "
            f"contain at most {MAX_NAME_LENGTH} ASCII letters, digits, '.', '_' or '-': {name!r}"
        )
    return name


def _validated_paths(paths: dict[str, Path]) -> list[tuple[str, Path]]:
    if not isinstance(paths, dict):
        raise StorageError("paths must be a dictionary of logical names to Path values")
    if len(paths) > MAX_PATH_COUNT:
        raise StorageError(f"at most {MAX_PATH_COUNT} paths may be sampled")
    validated: list[tuple[str, Path]] = []
    for name, path in paths.items():
        _validate_name(name)
        if not isinstance(path, Path):
            raise StorageError(f"path for {name!r} must be a pathlib.Path")
        validated.append((name, path))
    return sorted(validated)


def _filesystem_id(path: Path) -> str | None:
    """Return an opaque device identity without exposing a path."""
    try:
        device = os.stat(path, follow_symlinks=True).st_dev
    except OSError:
        return None
    digest = hashlib.sha256(VOLUME_ID_SALT + str(device).encode("ascii")).hexdigest()
    return f"filesystem-{digest[:16]}"


def _path_status(path: Path, *, reject_symlink: bool = False) -> str:
    try:
        if reject_symlink and path.is_symlink():
            return "unavailable"
        path.stat()
    except FileNotFoundError:
        return "missing"
    except OSError:
        return "unavailable"
    return "available"


def storage_snapshot(paths: dict[str, Path]) -> dict[str, Any]:
    """Return bounded filesystem-capacity evidence for explicit logical paths.

    Output contains only caller-provided logical names and numeric capacity
    values. Missing paths and capacity lookup failures have distinct statuses;
    neither is represented as zero. No recursive filesystem operation occurs.
    """
    entries: dict[str, dict[str, Any]] = {}
    filesystems: dict[str, dict[str, Any]] = {}
    for name, path in _validated_paths(paths):
        path_status = _path_status(path)
        if path_status != "available":
            entries[name] = {"status": path_status}
            continue
        identity = _filesystem_id(path)
        if identity is None:
            entries[name] = {"status": "unavailable"}
            continue
        filesystem_id = identity
        entries[name] = {"status": "unavailable", "filesystemId": filesystem_id}
        if filesystem_id in filesystems:
            entries[name]["status"] = filesystems[filesystem_id]["status"]
            continue
        try:
            usage = shutil.disk_usage(path)
        except (OSError, ValueError):
            filesystems[filesystem_id] = {"status": "unavailable"}
            continue
        filesystems[filesystem_id] = {
            "status": "available",
            "observedFreeBytes": usage.free,
            "totalBytes": usage.total,
        }
        entries[name]["status"] = "available"
    return {
        "schemaVersion": SCHEMA_VERSION,
        "attribution": "shared-host",
        "paths": entries,
        "filesystems": filesystems,
    }


def _absolute_path(path: Path) -> Path | None:
    try:
        # Resolve parent aliases for accounting; leaf symlinks were rejected.
        return path.resolve(strict=True)
    except (OSError, ValueError, RuntimeError):
        return None


def _overlap_owner(
    path: Path, filesystem_id: str | None, owners: list[tuple[str, Path, str | None]]
) -> str | None:
    """Find a previously measured lexical ancestor of ``path``."""
    for owner_name, owner_path, owner_filesystem in owners:
        if filesystem_id is None or filesystem_id != owner_filesystem:
            continue
        try:
            path.relative_to(owner_path)
        except ValueError:
            continue
        return owner_name
    return None


def _du_bytes(path: Path, timeout_seconds: float) -> int:
    if platform.system() == "Windows":
        raise AllocationMeasurementError("unavailable")
    executable = shutil.which("du")
    if executable is None:
        raise AllocationMeasurementError("unavailable")
    try:
        result = subprocess.run(
            [executable, "-skx", str(path)],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as error:
        raise AllocationMeasurementError("timeout") from error
    except OSError as error:
        raise AllocationMeasurementError("unavailable") from error
    if result.returncode != 0:
        raise AllocationMeasurementError("unavailable")
    fields = result.stdout.split()
    if not fields or not fields[0].isdigit():
        raise AllocationMeasurementError("unavailable")
    return int(fields[0]) * 1024


def allocated_snapshot(
    paths: dict[str, Path],
    *,
    timeout_seconds: float = DEFAULT_ALLOCATION_TIMEOUT_SECONDS,
    max_path_count: int = MAX_PATH_COUNT,
) -> dict[str, Any]:
    """Measure allocated bytes for explicit paths using bounded ``du`` calls.

    Parent/child and duplicate resolved paths are measured once. Leaf symlinks
    are rejected; parent aliases are resolved for accounting. Each result is keyed by its
    logical name and never includes the physical path. A missing path,
    unavailable ``du``, and an overlapping path are separate statuses.
    """
    if (
        not isinstance(timeout_seconds, (int, float))
        or isinstance(timeout_seconds, bool)
        or not math.isfinite(timeout_seconds)
        or timeout_seconds <= 0
        or timeout_seconds > MAX_ALLOCATION_TIMEOUT_SECONDS
    ):
        raise StorageError(
            f"allocation timeout must be between 0 and {MAX_ALLOCATION_TIMEOUT_SECONDS} seconds"
        )
    validated = _validated_paths(paths)
    if (
        not isinstance(max_path_count, int)
        or isinstance(max_path_count, bool)
        or max_path_count <= 0
        or max_path_count > MAX_PATH_COUNT
    ):
        raise StorageError(f"max_path_count must be between 1 and {MAX_PATH_COUNT}")
    if len(validated) > max_path_count:
        raise StorageError(f"at most {max_path_count} paths may be measured")

    results: dict[str, dict[str, Any]] = {}
    measured: list[tuple[str, Path, str | None]] = []
    total = 0
    available_count = 0
    unavailable_count = 0
    # Measure physical parents first, including paths through parent aliases.
    ordered = sorted(
        validated,
        key=lambda item: (len((_absolute_path(item[1]) or item[1]).parts), item[0]),
    )
    for name, path in ordered:
        path_status = _path_status(path, reject_symlink=True)
        if path_status != "available":
            results[name] = {"status": path_status, "allocatedBytes": None}
            unavailable_count += 1
            continue
        absolute = _absolute_path(path)
        if absolute is None:
            results[name] = {"status": "unavailable", "allocatedBytes": None}
            unavailable_count += 1
            continue
        filesystem_id = _filesystem_id(absolute)
        owner = _overlap_owner(absolute, filesystem_id, measured)
        if owner is not None:
            results[name] = {
                "status": "overlap",
                "allocatedBytes": None,
                "measuredAs": owner,
            }
            continue
        try:
            size = _du_bytes(absolute, float(timeout_seconds))
        except AllocationMeasurementError as error:
            results[name] = {"status": error.status, "allocatedBytes": None}
            unavailable_count += 1
            continue
        measured.append((name, absolute, filesystem_id))
        results[name] = {"status": "available", "allocatedBytes": size}
        total += size
        available_count += 1

    if not unavailable_count:
        status = "available"
    elif available_count:
        status = "partial"
    else:
        status = "unavailable"
    allocation: dict[str, Any] = {"status": status, "paths": results}
    if available_count:
        allocation["totalAllocatedBytes"] = total
    if unavailable_count:
        allocation["unavailablePathCount"] = unavailable_count
    return allocation


def parse_path_spec(spec: str) -> tuple[str, Path]:
    name, separator, raw_path = spec.partition("=")
    if not separator or not raw_path:
        raise argparse.ArgumentTypeError("path must use NAME=PATH syntax")
    try:
        _validate_name(name)
    except StorageError as error:
        raise argparse.ArgumentTypeError(str(error)) from error
    if "\0" in raw_path or "\n" in raw_path or "\r" in raw_path:
        raise argparse.ArgumentTypeError("path contains unsupported control characters")
    return name, Path(raw_path)


def parse_args(argv: Iterable[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--path",
        dest="path_specs",
        action="append",
        type=parse_path_spec,
        metavar="NAME=PATH",
        help="explicit classified path to sample (repeatable)",
    )
    parser.add_argument(
        "--allocated",
        action="store_true",
        help="also measure allocated bytes with bounded du calls",
    )
    arguments = parser.parse_args(argv)
    if not arguments.path_specs:
        parser.error("at least one explicit --path NAME=PATH is required")
    if len(arguments.path_specs) > MAX_PATH_COUNT:
        parser.error(f"at most {MAX_PATH_COUNT} --path values are supported")
    if len({name for name, _ in arguments.path_specs}) != len(arguments.path_specs):
        parser.error("--path names must be unique")
    return arguments


def main(argv: Iterable[str] | None = None) -> int:
    try:
        arguments = parse_args(argv)
        paths = dict(arguments.path_specs)
        report = storage_snapshot(paths)
        if arguments.allocated:
            report["allocation"] = allocated_snapshot(
                paths,
            )
        print(json.dumps(report, sort_keys=True))
    except StorageError as error:
        print(f"build-storage: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
