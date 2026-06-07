"""Distribution manifest generation for Codex Lab artifacts."""

import argparse
import hashlib
import json
from dataclasses import dataclass
from datetime import datetime
from datetime import timezone
from pathlib import Path
from typing import Any

from codex_package.version import read_workspace_version


SCHEMA_VERSION = 1
PRODUCT = "codex-lab"
CHANNEL = "lab"
PLATFORM = "aarch64-apple-darwin"
APP_ZIP = "codex-lab-app-aarch64-apple-darwin.zip"
SHIM_ZIP = "codex-lab-shim-aarch64-apple-darwin.zip"
MANIFEST_NAME = "codex-lab-distribution.json"


@dataclass(frozen=True)
class ArtifactSpec:
    role: str
    file_name: str
    archive_root: str
    description: str


ARTIFACTS = (
    ArtifactSpec(
        role="appZip",
        file_name=APP_ZIP,
        archive_root="Codex Lab.app",
        description="Canonical app update unit containing the embedded Codex Lab CLI.",
    ),
    ArtifactSpec(
        role="shimZip",
        file_name=SHIM_ZIP,
        archive_root="bin/codex-lab",
        description="Companion CLI wrapper that resolves an installed or sibling Codex Lab.app.",
    ),
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate or validate a Codex Lab distribution manifest.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    generate = subparsers.add_parser("generate", help="Write a distribution manifest.")
    generate.add_argument("--dist-dir", type=Path, required=True)
    generate.add_argument("--output", type=Path, required=True)
    generate.add_argument("--sha256sums", type=Path)
    generate.add_argument("--version")
    generate.add_argument("--bundle-version", required=True)
    generate.add_argument("--commit", required=True)
    generate.add_argument("--repository", required=True)
    generate.add_argument("--workflow", default="codex-lab-app")
    generate.add_argument("--run-id", required=True)
    generate.add_argument("--run-attempt", required=True)
    generate.add_argument("--generated-at")
    generate.set_defaults(func=cmd_generate)

    validate = subparsers.add_parser(
        "validate", help="Validate a distribution manifest."
    )
    validate.add_argument("manifest", type=Path)
    validate.add_argument("--dist-dir", type=Path)
    validate.add_argument("--sha256sums", type=Path)
    validate.set_defaults(func=cmd_validate)

    return parser.parse_args()


def main() -> int:
    args = parse_args()
    args.func(args)
    return 0


def cmd_generate(args: argparse.Namespace) -> None:
    sha256sums = args.sha256sums or args.dist_dir / "SHA256SUMS"
    checksums = read_sha256sums(sha256sums)
    manifest = build_manifest(
        dist_dir=args.dist_dir,
        checksums=checksums,
        version=args.version or read_workspace_version(),
        bundle_version=args.bundle_version,
        commit=args.commit,
        repository=args.repository,
        workflow=args.workflow,
        run_id=args.run_id,
        run_attempt=args.run_attempt,
        generated_at=args.generated_at,
    )
    validate_manifest(manifest, dist_dir=args.dist_dir, checksums=checksums)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8") as handle:
        json.dump(manifest, handle, indent=2, sort_keys=True)
        handle.write("\n")


def cmd_validate(args: argparse.Namespace) -> None:
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    checksums = read_sha256sums(args.sha256sums) if args.sha256sums else None
    validate_manifest(manifest, dist_dir=args.dist_dir, checksums=checksums)


def build_manifest(
    *,
    dist_dir: Path,
    checksums: dict[str, str],
    version: str,
    bundle_version: str,
    commit: str,
    repository: str,
    workflow: str,
    run_id: str,
    run_attempt: str,
    generated_at: str | None = None,
) -> dict[str, Any]:
    timestamp = generated_at or utc_timestamp()
    artifacts = {}
    for artifact in ARTIFACTS:
        path = dist_dir / artifact.file_name
        if not path.is_file():
            raise FileNotFoundError(f"Missing Codex Lab artifact: {path}")
        checksum = checksums.get(artifact.file_name)
        if checksum is None:
            raise ValueError(f"SHA256SUMS does not include {artifact.file_name}")
        actual_checksum = sha256_file(path)
        if checksum != actual_checksum:
            raise ValueError(
                f"Checksum mismatch for {artifact.file_name}: {checksum} != {actual_checksum}"
            )
        artifacts[artifact.role] = {
            "archiveRoot": artifact.archive_root,
            "description": artifact.description,
            "fileName": artifact.file_name,
            "notarized": False,
            "sha256": checksum,
            "signed": False,
            "sizeBytes": path.stat().st_size,
        }

    return {
        "artifacts": artifacts,
        "bundleVersion": bundle_version,
        "channel": CHANNEL,
        "desktopIntegration": {
            "cliOverrideEnv": "CODEX_CLI_PATH",
            "launchesFreshInstance": True,
            "officialCodexAppPath": "/Applications/Codex.app",
            "requiresOfficialCodexApp": True,
        },
        "generatedAt": timestamp,
        "platform": PLATFORM,
        "product": PRODUCT,
        "schemaVersion": SCHEMA_VERSION,
        "source": {
            "commit": commit,
            "repository": repository,
            "runAttempt": run_attempt,
            "runId": run_id,
            "workflow": workflow,
        },
        "supportedLayouts": [
            {
                "description": "Extract app and shim artifacts into the same root; bin/codex-lab resolves ../Codex Lab.app.",
                "kind": "extractedSibling",
            },
            {
                "description": "Install Codex Lab.app into /Applications or ~/Applications.",
                "kind": "applicationsFolder",
            },
            {
                "description": "Set CODEX_LAB_APP_PATH to the Codex Lab.app bundle path.",
                "kind": "envOverride",
            },
        ],
        "version": version,
    }


def utc_timestamp() -> str:
    return (
        datetime.now(timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def validate_manifest(
    manifest: dict[str, Any],
    *,
    dist_dir: Path | None = None,
    checksums: dict[str, str] | None = None,
) -> None:
    required_top_level = {
        "artifacts",
        "bundleVersion",
        "channel",
        "desktopIntegration",
        "generatedAt",
        "platform",
        "product",
        "schemaVersion",
        "source",
        "supportedLayouts",
        "version",
    }
    missing = sorted(required_top_level - manifest.keys())
    if missing:
        raise ValueError(f"Manifest is missing required fields: {missing}")
    if manifest["schemaVersion"] != SCHEMA_VERSION:
        raise ValueError(f"Unsupported schemaVersion: {manifest['schemaVersion']}")
    if manifest["product"] != PRODUCT:
        raise ValueError(f"Unexpected product: {manifest['product']}")
    if manifest["channel"] != CHANNEL:
        raise ValueError(f"Unexpected channel: {manifest['channel']}")
    if manifest["platform"] != PLATFORM:
        raise ValueError(f"Unexpected platform: {manifest['platform']}")
    validate_supported_layouts(manifest["supportedLayouts"])
    artifacts = manifest["artifacts"]
    if not isinstance(artifacts, dict):
        raise ValueError("Manifest artifacts must be an object")
    expected_roles = {artifact.role for artifact in ARTIFACTS}
    actual_roles = set(artifacts)
    if actual_roles != expected_roles:
        raise ValueError(
            f"Manifest artifact roles mismatch: {actual_roles} != {expected_roles}"
        )

    for artifact in ARTIFACTS:
        entry = artifacts[artifact.role]
        if not isinstance(entry, dict):
            raise ValueError(f"{artifact.role} must be an object")
        validate_artifact_entry(artifact, entry, dist_dir=dist_dir, checksums=checksums)


def validate_supported_layouts(layouts: object) -> None:
    if not isinstance(layouts, list):
        raise ValueError("Manifest supportedLayouts must be a list")
    expected_kinds = {"applicationsFolder", "envOverride", "extractedSibling"}
    actual_kinds = set()
    for layout in layouts:
        if not isinstance(layout, dict):
            raise ValueError("Manifest supportedLayouts entries must be objects")
        kind = layout.get("kind")
        description = layout.get("description")
        if not isinstance(kind, str) or not isinstance(description, str):
            raise ValueError(
                "Manifest supportedLayouts entries need kind and description"
            )
        actual_kinds.add(kind)
    if actual_kinds != expected_kinds:
        raise ValueError(
            f"Manifest supportedLayouts mismatch: {actual_kinds} != {expected_kinds}"
        )


def validate_artifact_entry(
    artifact: ArtifactSpec,
    entry: dict[str, Any],
    *,
    dist_dir: Path | None,
    checksums: dict[str, str] | None,
) -> None:
    required_fields = {
        "archiveRoot",
        "description",
        "fileName",
        "notarized",
        "sha256",
        "signed",
        "sizeBytes",
    }
    missing = sorted(required_fields - entry.keys())
    if missing:
        raise ValueError(f"{artifact.role} is missing required fields: {missing}")
    if entry["fileName"] != artifact.file_name:
        raise ValueError(
            f"{artifact.role} has unexpected fileName: {entry['fileName']}"
        )
    if entry["archiveRoot"] != artifact.archive_root:
        raise ValueError(
            f"{artifact.role} has unexpected archiveRoot: {entry['archiveRoot']}"
        )
    if entry["signed"] is not False or entry["notarized"] is not False:
        raise ValueError(f"{artifact.role} must be marked unsigned and not notarized")
    if not is_sha256(entry["sha256"]):
        raise ValueError(f"{artifact.role} has invalid sha256: {entry['sha256']}")
    if not isinstance(entry["sizeBytes"], int) or entry["sizeBytes"] <= 0:
        raise ValueError(f"{artifact.role} has invalid sizeBytes: {entry['sizeBytes']}")
    if checksums is not None and checksums.get(artifact.file_name) != entry["sha256"]:
        raise ValueError(f"{artifact.role} checksum does not match SHA256SUMS")
    if dist_dir is not None:
        path = dist_dir / artifact.file_name
        if not path.is_file():
            raise FileNotFoundError(f"Missing Codex Lab artifact: {path}")
        if path.stat().st_size != entry["sizeBytes"]:
            raise ValueError(f"{artifact.role} sizeBytes does not match {path}")
        if sha256_file(path) != entry["sha256"]:
            raise ValueError(f"{artifact.role} sha256 does not match {path}")


def read_sha256sums(path: Path) -> dict[str, str]:
    checksums = {}
    for line_number, line in enumerate(
        path.read_text(encoding="utf-8").splitlines(), start=1
    ):
        if not line.strip():
            continue
        parts = line.split()
        if len(parts) != 2:
            raise ValueError(f"Invalid SHA256SUMS line {line_number}: {line!r}")
        digest, file_name = parts
        if not is_sha256(digest):
            raise ValueError(f"Invalid SHA256 digest on line {line_number}: {digest}")
        checksums[file_name] = digest
    expected_names = {artifact.file_name for artifact in ARTIFACTS}
    actual_names = set(checksums)
    if actual_names != expected_names:
        raise ValueError(
            f"SHA256SUMS artifact mismatch: {actual_names} != {expected_names}"
        )
    return checksums


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as artifact:
        for chunk in iter(lambda: artifact.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_sha256(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(char in "0123456789abcdef" for char in value)
    )


if __name__ == "__main__":
    raise SystemExit(main())
