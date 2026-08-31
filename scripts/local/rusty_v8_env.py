#!/usr/bin/env python3
"""Resolve checksum-verified Codex rusty_v8 artifacts for local Cargo builds."""

import argparse
import fcntl
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
PROFILE = "ptrcomp_sandbox_release"
SAFE_REPOSITORY = re.compile(r"[A-Za-z0-9._-]+/[A-Za-z0-9._-]+\Z")


class RustyV8Error(Exception):
    """Raised when trusted local rusty_v8 artifacts cannot be resolved."""


def run_text(command: list[str], description: str) -> str:
    result = subprocess.run(
        command,
        cwd=REPO_ROOT,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        detail = (
            result.stderr.strip() or result.stdout.strip() or "no diagnostic output"
        )
        raise RustyV8Error(f"{description} failed: {detail}")
    return result.stdout.strip()


def resolved_version() -> str:
    return run_text(
        [
            sys.executable,
            str(REPO_ROOT / ".github" / "scripts" / "rusty_v8_bazel.py"),
            "resolved-v8-crate-version",
        ],
        "rusty_v8 version lookup",
    )


def host_target() -> str:
    output = run_text(["rustc", "-vV"], "Rust host lookup")
    for line in output.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ")
    raise RustyV8Error("rustc did not report a host target")


def asset_names(target: str) -> tuple[str, str]:
    if target.endswith("-pc-windows-msvc"):
        archive = f"rusty_v8_{PROFILE}_{target}.lib.gz"
    else:
        archive = f"librusty_v8_{PROFILE}_{target}.a.gz"
    return archive, f"src_binding_{PROFILE}_{target}.rs"


def manifest_checksums(version: str) -> dict[str, str]:
    checksums: dict[str, str] = {}
    manifest = (
        REPO_ROOT
        / "third_party"
        / "v8"
        / f"rusty_v8_{version.replace('.', '_')}_codex_release.sha256"
    )
    if not manifest.is_file():
        raise RustyV8Error(
            f"trusted checksum manifest is missing for rusty_v8 {version}"
        )
    for line in manifest.read_text(encoding="utf-8").splitlines():
        parts = line.split()
        if len(parts) == 2 and len(parts[0]) == 64:
            checksums[parts[1].lstrip("*")] = parts[0].lower()
    return checksums


def cache_root(environment: dict[str, str]) -> Path:
    override = environment.get("CODEX_LAB_RUSTY_V8_CACHE_DIR")
    if override:
        return Path(override).expanduser()
    artifact_root = environment.get("CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT")
    if (
        not environment.get("CODEX_LAB_RUSTY_V8_CACHE_DIR")
        and artifact_root
        and Path(artifact_root).is_dir()
        and os.access(artifact_root, os.W_OK)
    ):
        return Path(artifact_root) / "local" / "codex-lab" / "rusty-v8"
    cache_home = environment.get("XDG_CACHE_HOME")
    return (
        Path(cache_home).expanduser() / "codex-lab" / "rusty-v8"
        if cache_home
        else Path.home() / ".cache" / "codex-lab" / "rusty-v8"
    )


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_cached(path: Path, expected: str) -> bool:
    if not path.is_file():
        return False
    stamp = path.with_name(f"{path.name}.verified.json")
    stat = path.stat()
    if stamp.is_file():
        try:
            payload = json.loads(stamp.read_text(encoding="utf-8"))
            if payload == {
                "sha256": expected,
                "size": stat.st_size,
                "mtimeNs": stat.st_mtime_ns,
            }:
                return True
        except (OSError, json.JSONDecodeError):
            pass
    if sha256_file(path) != expected:
        path.unlink(missing_ok=True)
        stamp.unlink(missing_ok=True)
        return False
    stamp.write_text(
        json.dumps(
            {"sha256": expected, "size": stat.st_size, "mtimeNs": stat.st_mtime_ns},
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    return True


def inspect_cached(path: Path, expected: str) -> bool:
    return bool(expected) and path.is_file() and sha256_file(path) == expected


def download(url: str, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as handle:
        temporary = Path(handle.name)
    try:
        with (
            urllib.request.urlopen(url, timeout=120) as response,
            temporary.open("wb") as output,
        ):
            while chunk := response.read(1024 * 1024):
                output.write(chunk)
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)


def resolve(environment: dict[str, str]) -> dict[str, str]:
    version = resolved_version()
    target = host_target()
    archive_name, binding_name = asset_names(target)
    expected = manifest_checksums(version)
    if archive_name not in expected or binding_name not in expected:
        raise RustyV8Error(f"no trusted rusty_v8 artifacts are listed for {target}")
    repository = environment.get(
        "CODEX_LAB_RUSTY_V8_ARTIFACT_REPOSITORY", "openai/codex"
    )
    if SAFE_REPOSITORY.fullmatch(repository) is None:
        raise RustyV8Error("invalid rusty_v8 artifact repository")
    directory = cache_root(environment) / version / target
    directory.mkdir(parents=True, exist_ok=True)
    lock = directory / ".resolve.lock"
    with lock.open("w", encoding="utf-8") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        for name in (archive_name, binding_name):
            path = directory / name
            if not verify_cached(path, expected[name]):
                url = f"https://github.com/{repository}/releases/download/rusty-v8-v{version}/{name}"
                download(url, path)
                if not verify_cached(path, expected[name]):
                    raise RustyV8Error(
                        f"downloaded {name} failed checksum verification"
                    )
    return {
        "RUSTY_V8_ARCHIVE": str(directory / archive_name),
        "RUSTY_V8_SRC_BINDING_PATH": str(directory / binding_name),
    }


def status(environment: dict[str, str]) -> dict[str, Any]:
    version = resolved_version()
    target = host_target()
    archive_name, binding_name = asset_names(target)
    directory = cache_root(environment) / version / target
    expected = manifest_checksums(version)
    artifact_root = environment.get("CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT")
    cache_kind = (
        "custom" if environment.get("CODEX_LAB_RUSTY_V8_CACHE_DIR") else "user-cache"
    )
    if (
        not environment.get("CODEX_LAB_RUSTY_V8_CACHE_DIR")
        and artifact_root
        and Path(artifact_root).is_dir()
        and os.access(artifact_root, os.W_OK)
    ):
        cache_kind = "developer-artifacts"
    return {
        "schemaVersion": 1,
        "version": version,
        "target": target,
        "cacheKind": cache_kind,
        "archiveReady": inspect_cached(
            directory / archive_name, expected.get(archive_name, "")
        ),
        "bindingReady": inspect_cached(
            directory / binding_name, expected.get(binding_name, "")
        ),
        "host": platform.system(),
    }


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command", nargs="?", choices=("resolve", "status"), default="resolve"
    )
    parser.add_argument("--require", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        if args.command == "status":
            print(json.dumps(status(dict(os.environ)), indent=2, sort_keys=True))
        else:
            for key, value in resolve(dict(os.environ)).items():
                print(f"{key}={value}")
        return 0
    except Exception as error:
        print(f"rusty_v8 local setup: {error}", file=sys.stderr)
        return 2 if args.require else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
