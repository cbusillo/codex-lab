#!/usr/bin/env python3
"""Verify and expose the repository-built voice SDK to Cargo."""

import argparse
import hashlib
import json
import os
from pathlib import Path
from pathlib import PurePosixPath
import re
import shlex
import subprocess


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


IDENTITY_PATHS = (
    ".bazelrc",
    ".bazelversion",
    "MODULE.bazel",
    "MODULE.bazel.lock",
    "BUILD.bazel",
    # Include compiler and foreign-cc patch bytes, not just their module labels.
    "patches",
    "third_party/voice",
    ".github/scripts/voice_sdk_ci.py",
    ".github/actions/setup-voice-sdk/action.yml",
)
MODULES = (
    "gstreamer-1.0",
    "gstreamer-base-1.0",
    "gstreamer-app-1.0",
    "gstreamer-audio-1.0",
)


def identity(repository: Path, target: str, toolchain: str) -> str:
    value = hashlib.sha256(f"voice-sdk-v2\0{target}\0{toolchain}\0".encode())
    names = subprocess.check_output(
        [
            "git",
            "-C",
            repository,
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            *IDENTITY_PATHS,
        ]
    ).split(b"\0")
    tracked = sorted(name.decode() for name in names if name)
    for required in IDENTITY_PATHS:
        if not any(
            name == required or name.startswith(f"{required}/") for name in tracked
        ):
            raise ValueError(f"voice SDK identity input is missing: {required}")
    for name_text in tracked:
        item = repository / name_text
        if not item.is_file():
            raise ValueError(f"voice SDK identity input is not a file: {name_text}")
        name = name_text.encode()
        value.update(len(name).to_bytes(4, "big") + name)
        value.update(bytes.fromhex(digest(item)))
    return value.hexdigest()


def verify(sdk: Path, target: str, pkg_config: Path, sources: Path) -> None:
    receipt = sdk / "sdk.json"
    if (
        sdk.is_symlink()
        or not sdk.is_dir()
        or receipt.is_symlink()
        or not receipt.is_file()
        or receipt.stat().st_size > 2 * 1024 * 1024
    ):
        raise ValueError("voice SDK receipt must be a bounded regular file")
    metadata = json.loads(receipt.read_text())
    if (
        metadata.get("schemaVersion") != 1
        or metadata.get("target") != target
        or metadata.get("sourceManifestSha256") != digest(sources)
    ):
        raise ValueError("voice SDK metadata does not match the requested target")
    files = metadata.get("files")
    if (
        not re.fullmatch(r"[0-9a-f]{40}", metadata.get("sourceCommit", ""))
        or not isinstance(files, list)
        or not 1 <= len(files) <= 4096
    ):
        raise ValueError("voice SDK file inventory is invalid")
    expected = set()
    total = 0
    for record in files:
        name = record.get("path", "")
        relative = PurePosixPath(name)
        if relative.is_absolute() or ".." in relative.parts or name in expected:
            raise ValueError("voice SDK inventory path is invalid or duplicated")
        expected.add(name)
        path = sdk / name
        if (
            path.is_symlink()
            or not path.is_file()
            or not path.resolve(strict=True).is_relative_to(sdk.resolve())
        ):
            raise ValueError("voice SDK file escapes its root")
        total += path.stat().st_size
        if total > 512 * 1024 * 1024:
            raise ValueError("voice SDK exceeds its size limit")
        if digest(path) != record["sha256"]:
            raise ValueError(f"voice SDK digest mismatch: {record['path']}")
    actual = {
        path.relative_to(sdk).as_posix()
        for path in sdk.rglob("*")
        if path.is_file() or path.is_symlink()
    }
    if any(path.is_symlink() for path in sdk.rglob("*") if path.is_dir()):
        raise ValueError("voice SDK contains a symlinked directory")
    if actual != expected | {"sdk.json"}:
        raise ValueError("voice SDK contains an unverified file")
    libdir = sdk / "lib/pkgconfig"
    env = {
        "PATH": os.environ.get("PATH", ""),
        "PKG_CONFIG_LIBDIR": str(libdir),
        "PKG_CONFIG_PATH": "",
    }
    result = subprocess.run(
        [pkg_config, "--atleast-version=1.28", "gstreamer-1.0"], env=env
    )
    if result.returncode:
        raise ValueError("voice SDK does not provide GStreamer >= 1.28")
    flags = subprocess.check_output(
        [pkg_config, "--define-prefix", "--cflags", "--libs", *MODULES],
        env=env,
        text=True,
    )
    root = sdk.resolve()
    for flag in shlex.split(flags):
        if flag.startswith(("-I", "-L")) and not Path(
            flag[2:]
        ).resolve().is_relative_to(root):
            raise ValueError(f"pkg-config escaped the voice SDK: {flag}")


def main() -> None:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    key = sub.add_parser("identity")
    key.add_argument("--repository", type=Path, required=True)
    key.add_argument("--target", required=True)
    key.add_argument("--toolchain", required=True)
    check = sub.add_parser("verify")
    check.add_argument("--sdk", type=Path, required=True)
    check.add_argument("--target", required=True)
    check.add_argument("--pkg-config", type=Path, required=True)
    check.add_argument("--sources", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "identity":
        print(identity(args.repository, args.target, args.toolchain))
    else:
        verify(args.sdk, args.target, args.pkg_config, args.sources)


if __name__ == "__main__":
    main()
