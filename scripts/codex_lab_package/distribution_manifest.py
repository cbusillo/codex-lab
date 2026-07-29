"""Distribution manifest generation for Codex Lab artifacts."""

import argparse
import hashlib
import json
import stat
import zipfile
from dataclasses import dataclass
from datetime import datetime
from datetime import timezone
from pathlib import Path
from typing import Any

from codex_lab_package.layout import DEFAULT_CODEX_APP_PATH
from codex_lab_package.layout import MAX_PROVENANCE_BYTES
from codex_lab_package.layout import OFFICIAL_APP_BUNDLE_IDENTIFIER
from codex_lab_package.layout import OFFICIAL_APP_CANDIDATE_PATHS
from codex_lab_package.layout import OFFICIAL_APP_TEAM_IDENTIFIER
from codex_lab_package.engine_contract import ENGINE_ARCHIVE_ROOT
from codex_lab_package.engine_contract import ENGINE_SIGNING_IDENTIFIER
from codex_lab_package.engine_contract import ENGINE_TEAM_IDENTIFIER
from codex_lab_package.engine_contract import REQUIRED_ENGINE_ENTITLEMENTS
from codex_lab_package.release_tag import release_version_from_tag
from codex_package.version import read_workspace_version


SCHEMA_VERSION = 2
PRODUCT = "codex-lab"
CHANNEL = "lab"
PLATFORM = "aarch64-apple-darwin"
APP_ZIP = "codex-lab-app-aarch64-apple-darwin.zip"
SHIM_ZIP = "codex-lab-shim-aarch64-apple-darwin.zip"
ENGINE_ZIP = "codex-lab-engine-aarch64-apple-darwin.zip"
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
    ArtifactSpec(
        role="engineZip",
        file_name=ENGINE_ZIP,
        archive_root=ENGINE_ARCHIVE_ROOT,
        description="Individually signed managed engine pinned by the Codex Lab supervisor.",
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
    generate.add_argument("--release-tag")
    generate.add_argument("--download-base-url")
    generate.add_argument("--generated-at")
    generate.add_argument("--engine-signed", action="store_true")
    generate.add_argument("--engine-notarized", action="store_true")
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
        release_tag=args.release_tag,
        download_base_url=args.download_base_url,
        generated_at=args.generated_at,
        engine_signed=args.engine_signed,
        engine_notarized=args.engine_notarized,
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
    release_tag: str | None = None,
    download_base_url: str | None = None,
    generated_at: str | None = None,
    engine_signed: bool = False,
    engine_notarized: bool = False,
) -> dict[str, Any]:
    if engine_notarized and not engine_signed:
        raise ValueError("The managed engine cannot be notarized unless it is signed")
    timestamp = generated_at or utc_timestamp()
    base_url = normalize_download_base_url(download_base_url)
    if (release_tag is None) != (base_url is None):
        raise ValueError("release tag and download base URL must be provided together")
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
        is_engine = artifact.role == "engineZip"
        artifacts[artifact.role] = {
            "archiveRoot": artifact.archive_root,
            "description": artifact.description,
            "fileName": artifact.file_name,
            "notarized": engine_notarized if is_engine else False,
            "sha256": checksum,
            "signed": engine_signed if is_engine else False,
            "sizeBytes": path.stat().st_size,
        }
        if base_url is not None:
            artifacts[artifact.role]["downloadUrl"] = f"{base_url}/{artifact.file_name}"

    manifest = {
        "artifacts": artifacts,
        "bundleVersion": bundle_version,
        "channel": CHANNEL,
        "desktopIntegration": {
            "cliOverrideEnv": "CODEX_CLI_PATH",
            "existingAppPolicy": "failClosed",
            "launchesFreshInstance": True,
            "officialAppBundleIdentifier": OFFICIAL_APP_BUNDLE_IDENTIFIER,
            "officialAppPaths": list(OFFICIAL_APP_CANDIDATE_PATHS),
            "officialAppTeamIdentifier": OFFICIAL_APP_TEAM_IDENTIFIER,
            "officialCodexAppPath": str(DEFAULT_CODEX_APP_PATH),
            "provenanceCommand": ["debug", "provenance", "--json"],
            "provenanceMaxBytes": MAX_PROVENANCE_BYTES,
            "requiresOfficialAppNotRunning": True,
            "requiresOfficialCodexApp": True,
            "requiresValidOfficialSignature": True,
        },
        "generatedAt": timestamp,
        "managedEngine": {
            "artifactRole": "engineZip",
            "requiredEntitlements": list(REQUIRED_ENGINE_ENTITLEMENTS),
            "sha256": sha256_zip_member(
                dist_dir / ENGINE_ZIP,
                ENGINE_ARCHIVE_ROOT,
            ),
            "signingIdentifier": ENGINE_SIGNING_IDENTIFIER if engine_signed else None,
            "sourceCommit": commit,
            "teamIdentifier": ENGINE_TEAM_IDENTIFIER if engine_signed else None,
            "version": version,
        },
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
    if release_tag is not None:
        manifest["release"] = {
            "tag": release_tag,
        }
    return manifest


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
        "managedEngine",
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
    version = manifest["version"]
    if not isinstance(version, str) or not version:
        raise ValueError("Manifest version must be a non-empty string")
    validate_release(manifest.get("release"), version)
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
    validate_managed_engine(
        manifest["managedEngine"],
        artifacts["engineZip"],
        version=version,
        source=manifest["source"],
        release=manifest.get("release"),
        dist_dir=dist_dir,
    )
    validate_release_download_urls(manifest.get("release"), artifacts)


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


def validate_release(release: object, version: str) -> None:
    if release is None:
        return
    if not isinstance(release, dict):
        raise ValueError("Manifest release must be an object")
    tag = release.get("tag")
    if not isinstance(tag, str) or not tag:
        raise ValueError("Manifest release tag must be a non-empty string")
    tag_version = release_version_from_tag(tag)
    if tag_version != version:
        raise ValueError(
            f"Manifest release tag version does not match manifest version: {tag} != {version}"
        )


def validate_release_download_urls(
    release: object,
    artifacts: dict[str, object],
) -> None:
    has_release = release is not None
    missing_download_url_roles = [
        role
        for role, entry in artifacts.items()
        if not isinstance(entry, dict) or entry.get("downloadUrl") is None
    ]
    has_download_urls = len(missing_download_url_roles) != len(artifacts)
    if not has_release and has_download_urls:
        raise ValueError(
            "Manifest release metadata and artifact download URLs must be provided together"
        )
    if has_release and not has_download_urls:
        raise ValueError(
            "Manifest release metadata and artifact download URLs must be provided together"
        )
    if has_release and missing_download_url_roles:
        raise ValueError(
            "Manifest release metadata requires download URLs for every artifact: "
            f"{sorted(missing_download_url_roles)}"
        )
    if isinstance(release, dict):
        tag = release["tag"]
        for role, entry in artifacts.items():
            if not isinstance(entry, dict):
                raise ValueError(f"{role} must be an object")
            file_name = entry["fileName"]
            download_url = entry["downloadUrl"]
            expected_suffix = f"/releases/download/{tag}/{file_name}"
            if not download_url.endswith(expected_suffix):
                raise ValueError(
                    f"{role} downloadUrl must target release artifact: {download_url}"
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
    signed = entry["signed"]
    notarized = entry["notarized"]
    if not isinstance(signed, bool) or not isinstance(notarized, bool):
        raise ValueError(f"{artifact.role} signing fields must be booleans")
    if artifact.role != "engineZip" and (signed or notarized):
        raise ValueError(f"{artifact.role} must be marked unsigned and not notarized")
    if artifact.role == "engineZip" and notarized and not signed:
        raise ValueError("engineZip cannot be notarized unless it is signed")
    if not is_sha256(entry["sha256"]):
        raise ValueError(f"{artifact.role} has invalid sha256: {entry['sha256']}")
    if not isinstance(entry["sizeBytes"], int) or entry["sizeBytes"] <= 0:
        raise ValueError(f"{artifact.role} has invalid sizeBytes: {entry['sizeBytes']}")
    download_url = entry.get("downloadUrl")
    if download_url is not None and not is_https_url(download_url):
        raise ValueError(f"{artifact.role} has invalid downloadUrl: {download_url}")
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


def validate_managed_engine(
    managed_engine: object,
    engine_artifact: dict[str, Any],
    *,
    version: str,
    source: object,
    release: object,
    dist_dir: Path | None,
) -> None:
    if not isinstance(managed_engine, dict):
        raise ValueError("Manifest managedEngine must be an object")
    required_fields = {
        "artifactRole",
        "requiredEntitlements",
        "sha256",
        "signingIdentifier",
        "sourceCommit",
        "teamIdentifier",
        "version",
    }
    missing = sorted(required_fields - managed_engine.keys())
    if missing:
        raise ValueError(f"managedEngine is missing required fields: {missing}")
    if managed_engine["artifactRole"] != "engineZip":
        raise ValueError("managedEngine artifactRole must be engineZip")
    if managed_engine["version"] != version:
        raise ValueError("managedEngine version must match the manifest version")
    if not isinstance(source, dict) or managed_engine["sourceCommit"] != source.get(
        "commit"
    ):
        raise ValueError("managedEngine sourceCommit must match the manifest source")
    if not is_sha256(managed_engine["sha256"]):
        raise ValueError("managedEngine sha256 must be a lowercase SHA-256 digest")
    if managed_engine["requiredEntitlements"] != list(REQUIRED_ENGINE_ENTITLEMENTS):
        raise ValueError("managedEngine requiredEntitlements are invalid")

    if engine_artifact["signed"]:
        if managed_engine["signingIdentifier"] != ENGINE_SIGNING_IDENTIFIER:
            raise ValueError("managedEngine signingIdentifier is invalid")
        if managed_engine["teamIdentifier"] != ENGINE_TEAM_IDENTIFIER:
            raise ValueError("managedEngine teamIdentifier is invalid")
    elif (
        managed_engine["signingIdentifier"] is not None
        or managed_engine["teamIdentifier"] is not None
    ):
        raise ValueError(
            "Unsigned managedEngine metadata must not claim a signing identity"
        )

    if release is not None and not engine_artifact["signed"]:
        raise ValueError("Published releases require a signed managed engine")
    if dist_dir is not None:
        actual_sha256 = sha256_zip_member(
            dist_dir / ENGINE_ZIP,
            ENGINE_ARCHIVE_ROOT,
        )
        if managed_engine["sha256"] != actual_sha256:
            raise ValueError("managedEngine sha256 does not match the engine archive")


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


def sha256_zip_member(path: Path, member_name: str) -> str:
    with zipfile.ZipFile(path) as archive:
        members = [info for info in archive.infolist() if not info.is_dir()]
        if len(members) != 1 or members[0].filename != member_name:
            raise ValueError(
                f"Engine archive must contain exactly {member_name}: {path}"
            )
        mode = (members[0].external_attr >> 16) & 0o777777
        if mode and not stat.S_ISREG(mode):
            raise ValueError(f"Engine archive member must be a regular file: {path}")
        digest = hashlib.sha256()
        with archive.open(members[0]) as artifact:
            for chunk in iter(lambda: artifact.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()


def normalize_download_base_url(url: str | None) -> str | None:
    if url is None:
        return None
    if not is_https_url(url):
        raise ValueError(f"download base URL must be an HTTPS URL: {url}")
    return url.rstrip("/")


def is_https_url(value: object) -> bool:
    return isinstance(value, str) and value.startswith("https://") and len(value) > 8


def is_sha256(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(char in "0123456789abcdef" for char in value)
    )


if __name__ == "__main__":
    raise SystemExit(main())
