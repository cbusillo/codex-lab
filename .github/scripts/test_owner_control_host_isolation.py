#!/usr/bin/env python3

"""Guard the inert owner-control contract and host dependency boundary."""

import sys
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CARGO_ROOT = ROOT / "codex-rs"
CONTRACT = "codex-owner-control-contract"
HOST = "codex-owner-control-host"
HOST_MANIFEST = CARGO_ROOT / "owner-control-host" / "Cargo.toml"
ALLOWED_HOST_DEPENDENCIES = {CONTRACT, "serde_json", "time"}
ALLOWED_HOST_DEV_DEPENDENCIES = {"base64", "ed25519-dalek", "pretty_assertions"}
DENIED_RUNTIME_DEPENDENCIES = {
    "codex-app-server",
    "codex-browser",
    "codex-code-bridge-client",
    "codex-code-bridge-protocol",
    "codex-code-bridge-service",
    "codex-config",
    "codex-core",
    "codex-keyring-store",
    "codex-mcp",
    "codex-mcp-server",
    "codex-tools",
    "codex-tui",
    "keyring",
    "log",
    "reqwest",
    "socket2",
    "tokio",
    "tracing",
}


def dependency_names(manifest: dict[str, object], section: str) -> set[str]:
    dependencies = manifest.get(section, {})
    if not isinstance(dependencies, dict):
        return set()
    return set(dependencies)


def manifests() -> list[tuple[Path, dict[str, object]]]:
    found = []
    for path in CARGO_ROOT.rglob("Cargo.toml"):
        with path.open("rb") as file:
            manifest = tomllib.load(file)
        if isinstance(manifest.get("package"), dict):
            found.append((path, manifest))
    return found


def check_isolation() -> int:
    errors = []
    all_manifests = manifests()
    for path, manifest in all_manifests:
        package = manifest["package"]
        assert isinstance(package, dict)
        package_name = package.get("name")
        dependencies = set().union(
            dependency_names(manifest, "dependencies"),
            dependency_names(manifest, "dev-dependencies"),
            dependency_names(manifest, "build-dependencies"),
        )
        if CONTRACT in dependencies and package_name != HOST:
            errors.append(f"{path.relative_to(ROOT)} depends on {CONTRACT}")
        if HOST in dependencies:
            errors.append(f"{path.relative_to(ROOT)} depends on {HOST}")

    with HOST_MANIFEST.open("rb") as file:
        host_manifest = tomllib.load(file)
    host_dependencies = dependency_names(host_manifest, "dependencies")
    unexpected = host_dependencies - ALLOWED_HOST_DEPENDENCIES
    if unexpected:
        errors.append(
            "host has unexpected production dependencies: "
            + ", ".join(sorted(unexpected))
        )
    denied = host_dependencies & DENIED_RUNTIME_DEPENDENCIES
    if denied:
        errors.append(
            "host has denied runtime dependencies: " + ", ".join(sorted(denied))
        )
    if dependency_names(host_manifest, "build-dependencies"):
        errors.append("host must not have build dependencies")
    unexpected_dev = dependency_names(host_manifest, "dev-dependencies") - ALLOWED_HOST_DEV_DEPENDENCIES
    if unexpected_dev:
        errors.append(
            "host has unexpected dev dependencies: "
            + ", ".join(sorted(unexpected_dev))
        )

    if errors:
        sys.stdout.write("owner-control isolation violations:\n")
        sys.stdout.write("".join(f"- {error}\n" for error in errors))
        return 1
    return 0


class OwnerControlHostIsolationTest(unittest.TestCase):
    def test_inert_boundary_is_preserved(self) -> None:
        self.assertEqual(check_isolation(), 0)


if __name__ == "__main__":
    unittest.main()
