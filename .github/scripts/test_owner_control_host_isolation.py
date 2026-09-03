#!/usr/bin/env python3

"""Guard the inert owner-control contract, host, and IPC dependency boundary."""

import sys
import tomllib
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CARGO_ROOT = ROOT / "codex-rs"
CONTRACT = "codex-owner-control-contract"
HOST = "codex-owner-control-host"
IPC = "codex-owner-control-ipc"
HOST_MANIFEST = CARGO_ROOT / "owner-control-host" / "Cargo.toml"
IPC_MANIFEST = CARGO_ROOT / "owner-control-ipc" / "Cargo.toml"
ALLOWED_HOST_DEPENDENCIES = {CONTRACT, "serde_json", "time"}
ALLOWED_HOST_DEV_DEPENDENCIES = {"base64", "ed25519-dalek", "pretty_assertions"}
ALLOWED_IPC_DEPENDENCIES = {CONTRACT, HOST, "libc", "serde", "serde_json"}
ALLOWED_IPC_DEV_DEPENDENCIES = {"pretty_assertions", "tempfile"}
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
DENIED_IPC_SOURCE_SYMBOLS = {
    "ConfirmedOwnerControlEnvelope",
    "OwnerControlConfirmationEnvelope",
    "OwnerControlReplayStore",
    "OwnerSigningCustody",
    "include!",
    "std::env",
    "std::process",
}
DENIED_HOST_SOURCE_SYMBOLS = {
    "Deserialize",
    "Serialize",
    "std::env",
    "std::fs",
    "std::net",
    "std::process",
}
PUBLIC_OBSERVATION_FIELDS = {
    "pub channel_binding:",
    "pub channel_binding_sha256:",
    "pub observed_host:",
    "pub principal_claim:",
    "pub principal_claim_sha256:",
    "pub provenance_tier:",
    "pub server_observed_corroboration:",
}


def dependency_package_names(
    dependencies: object,
    workspace_dependencies: dict[str, str] | None = None,
) -> set[str]:
    if not isinstance(dependencies, dict):
        return set()
    names = set()
    for alias, dependency in dependencies.items():
        if isinstance(dependency, dict) and dependency.get("workspace") is True:
            names.add((workspace_dependencies or {}).get(alias, alias))
        elif isinstance(dependency, dict) and isinstance(dependency.get("package"), str):
            names.add(dependency["package"])
        else:
            names.add(alias)
    return names


def dependency_names(
    manifest: dict[str, object],
    section: str,
    workspace_dependencies: dict[str, str] | None = None,
) -> set[str]:
    names = dependency_package_names(
        manifest.get(section, {}), workspace_dependencies
    )
    targets = manifest.get("target", {})
    if not isinstance(targets, dict):
        return names
    for target in targets.values():
        if isinstance(target, dict):
            names.update(
                dependency_package_names(
                    target.get(section, {}), workspace_dependencies
                )
            )
    return names


def all_dependency_names(
    manifest: dict[str, object],
    workspace_dependencies: dict[str, str] | None = None,
) -> set[str]:
    return set().union(
        dependency_names(manifest, "dependencies", workspace_dependencies),
        dependency_names(manifest, "dev-dependencies", workspace_dependencies),
        dependency_names(manifest, "build-dependencies", workspace_dependencies),
    )


def workspace_dependency_names() -> dict[str, str]:
    with (CARGO_ROOT / "Cargo.toml").open("rb") as file:
        manifest = tomllib.load(file)
    workspace = manifest.get("workspace", {})
    if not isinstance(workspace, dict):
        return {}
    dependencies = workspace.get("dependencies", {})
    if not isinstance(dependencies, dict):
        return {}
    return {
        alias: next(iter(dependency_package_names({alias: dependency})))
        for alias, dependency in dependencies.items()
    }


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
    workspace_dependencies = workspace_dependency_names()
    for path, manifest in all_manifests:
        package = manifest["package"]
        assert isinstance(package, dict)
        package_name = package.get("name")
        dependencies = all_dependency_names(manifest, workspace_dependencies)
        if CONTRACT in dependencies and package_name not in {HOST, IPC}:
            errors.append(f"{path.relative_to(ROOT)} depends on {CONTRACT}")
        if HOST in dependencies and package_name != IPC:
            errors.append(f"{path.relative_to(ROOT)} depends on {HOST}")
        if IPC in dependencies:
            errors.append(f"{path.relative_to(ROOT)} depends on {IPC}")

    with HOST_MANIFEST.open("rb") as file:
        host_manifest = tomllib.load(file)
    host_dependencies = dependency_names(
        host_manifest, "dependencies", workspace_dependencies
    )
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
    if dependency_names(host_manifest, "build-dependencies", workspace_dependencies):
        errors.append("host must not have build dependencies")
    unexpected_dev = (
        dependency_names(host_manifest, "dev-dependencies", workspace_dependencies)
        - ALLOWED_HOST_DEV_DEPENDENCIES
    )
    if unexpected_dev:
        errors.append(
            "host has unexpected dev dependencies: "
            + ", ".join(sorted(unexpected_dev))
        )
    host_source_root = CARGO_ROOT / "owner-control-host" / "src"
    host_source = "\n".join(
        path.read_text()
        for path in sorted(host_source_root.rglob("*.rs"))
        if path.name != "tests.rs" and not path.name.endswith("_tests.rs")
    )
    forbidden_host_symbols = sorted(
        symbol for symbol in DENIED_HOST_SOURCE_SYMBOLS if symbol in host_source
    )
    if forbidden_host_symbols:
        errors.append(
            "host source contains denied observation capability symbols: "
            + ", ".join(forbidden_host_symbols)
        )
    public_observation_fields = sorted(
        field for field in PUBLIC_OBSERVATION_FIELDS if field in host_source
    )
    if public_observation_fields:
        errors.append(
            "host observed descriptors expose public construction fields: "
            + ", ".join(public_observation_fields)
        )
    if (
        (host_source_root / "main.rs").exists()
        or (host_source_root / "bin").exists()
        or host_manifest.get("bin")
    ):
        errors.append("host must not define a binary entry point")
    host_package = host_manifest.get("package", {})
    if (
        (CARGO_ROOT / "owner-control-host" / "build.rs").exists()
        or isinstance(host_package, dict)
        and host_package.get("build") not in {None, False}
    ):
        errors.append("host must not define a build script")
    host_library = host_manifest.get("lib", {})
    if not isinstance(host_library, dict) or host_library.get("path") != "src/lib.rs":
        errors.append("host library path must remain src/lib.rs")

    with IPC_MANIFEST.open("rb") as file:
        ipc_manifest = tomllib.load(file)
    ipc_dependencies = dependency_names(
        ipc_manifest, "dependencies", workspace_dependencies
    )
    unexpected = ipc_dependencies - ALLOWED_IPC_DEPENDENCIES
    if unexpected:
        errors.append(
            "IPC has unexpected production dependencies: "
            + ", ".join(sorted(unexpected))
        )
    denied = ipc_dependencies & DENIED_RUNTIME_DEPENDENCIES
    if denied:
        errors.append(
            "IPC has denied runtime dependencies: " + ", ".join(sorted(denied))
        )
    if dependency_names(ipc_manifest, "build-dependencies", workspace_dependencies):
        errors.append("IPC must not have build dependencies")
    unexpected_dev = (
        dependency_names(ipc_manifest, "dev-dependencies", workspace_dependencies)
        - ALLOWED_IPC_DEV_DEPENDENCIES
    )
    if unexpected_dev:
        errors.append(
            "IPC has unexpected dev dependencies: "
            + ", ".join(sorted(unexpected_dev))
        )
    ipc_source_root = CARGO_ROOT / "owner-control-ipc" / "src"
    if (
        (ipc_source_root / "main.rs").exists()
        or (ipc_source_root / "bin").exists()
        or ipc_manifest.get("bin")
    ):
        errors.append("IPC must not define a binary entry point")
    ipc_package = ipc_manifest.get("package", {})
    if (
        (CARGO_ROOT / "owner-control-ipc" / "build.rs").exists()
        or isinstance(ipc_package, dict)
        and ipc_package.get("build") not in {None, False}
    ):
        errors.append("IPC must not define a build script")
    ipc_library = ipc_manifest.get("lib", {})
    if not isinstance(ipc_library, dict) or ipc_library.get("path") != "src/lib.rs":
        errors.append("IPC library path must remain src/lib.rs")
    ipc_source = "\n".join(
        path.read_text()
        for path in sorted(ipc_source_root.rglob("*.rs"))
    )
    forbidden_symbols = sorted(
        symbol for symbol in DENIED_IPC_SOURCE_SYMBOLS if symbol in ipc_source
    )
    if forbidden_symbols:
        errors.append(
            "IPC source contains denied authority symbols: "
            + ", ".join(forbidden_symbols)
        )

    if errors:
        sys.stdout.write("owner-control isolation violations:\n")
        sys.stdout.write("".join(f"- {error}\n" for error in errors))
        return 1
    return 0


class OwnerControlHostIsolationTest(unittest.TestCase):
    def test_dependency_names_resolve_aliases_and_target_sections(self) -> None:
        manifest = {
            "dependencies": {"alias": {"package": "direct-package"}},
            "target": {
                "cfg(unix)": {
                    "dependencies": {
                        "target-alias": {"package": "target-package"}
                    }
                }
            },
        }
        self.assertEqual(
            dependency_names(manifest, "dependencies"),
            {"direct-package", "target-package"},
        )

    def test_dependency_names_resolve_workspace_aliases(self) -> None:
        manifest = {
            "dependencies": {"stealth-ipc": {"workspace": True}},
        }
        self.assertEqual(
            dependency_names(
                manifest,
                "dependencies",
                {"stealth-ipc": IPC},
            ),
            {IPC},
        )

    def test_inert_boundary_is_preserved(self) -> None:
        self.assertEqual(check_isolation(), 0)


if __name__ == "__main__":
    unittest.main()
