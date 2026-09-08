#!/usr/bin/env python3
"""Check configured free-space floors for local build storage."""

import argparse
import os
import stat
import sys
from pathlib import Path

if sys.version_info < (3, 10):
    print("storage admission requires Python 3.10 or newer", file=sys.stderr)
    raise SystemExit(1)


MAX_FREE_BYTES = (1 << 63) - 1
MAX_PARENT_STEPS = 128
MAX_FLOOR_DIGITS = len(str(MAX_FREE_BYTES))


class AdmissionError(Exception):
    """Raised when a storage admission check cannot prove its inputs safe."""


def parse_floor(value: str | None, name: str) -> int | None:
    if value is None:
        return None
    if (
        not value
        or not value.isascii()
        or not value.isdecimal()
        or len(value) > MAX_FLOOR_DIGITS
    ):
        raise AdmissionError(f"{name} must be a canonical positive decimal")
    if value[0] == "0":
        raise AdmissionError(f"{name} must be a canonical positive decimal")
    result = int(value)
    if result > MAX_FREE_BYTES:
        raise AdmissionError(f"{name} exceeds the supported maximum")
    return result


def canonical_existing_directory(path: str, name: str) -> str:
    raw_path = os.fspath(path)
    if not os.path.isabs(raw_path):
        raw_path = os.path.join(os.getcwd(), raw_path)
    candidate = raw_path
    missing_parts: list[str] = []
    for _ in range(MAX_PARENT_STEPS):
        try:
            info = os.lstat(candidate)
        except FileNotFoundError:
            parent = os.path.dirname(candidate)
            if parent == candidate:
                raise AdmissionError(f"{name} parent walk reached filesystem root")
            missing_parts.append(os.path.basename(candidate))
            candidate = parent
            continue
        except OSError as error:
            raise AdmissionError(f"{name} cannot be canonicalized: {error}") from error
        if any(part == ".." for part in missing_parts):
            raise AdmissionError(
                f"{name} has an ambiguous missing prefix before '..': {candidate}"
            )
        if not stat.S_ISDIR(info.st_mode) and not stat.S_ISLNK(info.st_mode):
            raise AdmissionError(
                f"{name} resolves through a non-directory: {candidate}"
            )
        try:
            resolved = str(Path(candidate).resolve(strict=True))
            resolved_info = os.stat(resolved)
        except (OSError, RuntimeError) as error:
            raise AdmissionError(f"{name} cannot be canonicalized: {error}") from error
        if not stat.S_ISDIR(resolved_info.st_mode):
            raise AdmissionError(
                f"{name} canonical path is not a directory: {resolved}"
            )
        return resolved
    raise AdmissionError(f"{name} parent walk exceeded {MAX_PARENT_STEPS} components")


def available_bytes(path: str, name: str) -> int:
    statvfs = getattr(os, "statvfs", None)
    if statvfs is None:
        raise AdmissionError(f"{name} native POSIX capacity is unavailable")
    try:
        usage = statvfs(path)
    except OSError as error:
        raise AdmissionError(f"{name} capacity is unknown: {error}") from error
    if usage.f_bavail < 0 or usage.f_frsize <= 0:
        raise AdmissionError(f"{name} capacity is unknown")
    return usage.f_bavail * usage.f_frsize


def admit(
    root_path: str,
    target_path: str,
    min_root_free: str | None,
    min_target_free: str | None,
) -> None:
    root_floor = parse_floor(min_root_free, "CODEX_LAB_STORAGE_MIN_ROOT_FREE_BYTES")
    target_floor = parse_floor(
        min_target_free, "CODEX_LAB_STORAGE_MIN_TARGET_FREE_BYTES"
    )
    root = canonical_existing_directory(root_path, "root storage")
    target = canonical_existing_directory(target_path, "target storage")
    if root_floor is not None:
        root_free = available_bytes(root, "root storage")
        if root_free < root_floor:
            raise AdmissionError(
                f"root storage has {root_free} bytes available, below {root_floor}"
            )
    if target_floor is not None:
        target_free = available_bytes(target, "target storage")
        if target_free < target_floor:
            raise AdmissionError(
                f"target storage has {target_free} bytes available, "
                f"below {target_floor}"
            )


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root-path", required=True)
    parser.add_argument("--target-path", required=True)
    parser.add_argument("--min-root-free")
    parser.add_argument("--min-target-free")
    args = parser.parse_args(argv)
    try:
        admit(
            args.root_path,
            args.target_path,
            args.min_root_free,
            args.min_target_free,
        )
    except AdmissionError as error:
        print(f"storage admission: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
