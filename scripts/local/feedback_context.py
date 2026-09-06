"""Bounded build-input fingerprints; declarations are not inferred tool settings."""

import hashlib
import json
import os
from pathlib import Path


BUILD_ENVIRONMENT_KEYS = (
    "CARGO_BUILD_TARGET",
    "CARGO_BUILD_JOBS",
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE_DEV_DEBUG",
    "CARGO_PROFILE_DEV_INCREMENTAL",
    "CARGO_PROFILE_RELEASE_DEBUG",
    "CARGO_PROFILE_RELEASE_LTO",
    "CARGO_TARGET_DIR",
    "CODEX_LAB_CARGO_TARGET_DIR",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTC_WRAPPER",
    "SCCACHE_DIR",
    "SCCACHE_CACHE_SIZE",
    "SCCACHE_ENDPOINT",
)


def fingerprint(value: object) -> str:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(b"codex-build-context-v1\0" + encoded).hexdigest()


def build_context(repo_root: Path, command: list[str], configuration: str) -> dict:
    files = {}
    for label, relative in (
        ("toolchain", "codex-rs/rust-toolchain.toml"),
        ("lockfile", "codex-rs/Cargo.lock"),
    ):
        try:
            files[label] = hashlib.sha256(
                (repo_root / relative).read_bytes()
            ).hexdigest()
        except OSError:
            files[label] = None
    environment = {
        key: os.environ[key] for key in BUILD_ENVIRONMENT_KEYS if key in os.environ
    }
    return {
        "configuration": configuration,
        "configurationSource": "caller-declared",
        "invocationFingerprint": fingerprint(command),
        "environmentFingerprint": fingerprint(environment),
        "environmentKeys": sorted(environment),
        "sourcePathFingerprint": fingerprint(str(repo_root.resolve())),
        "fileFingerprints": files,
        "scope": "invocation-and-selected-environment; effective-tool-settings-not-inferred",
    }
