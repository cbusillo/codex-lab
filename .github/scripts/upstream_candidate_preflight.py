#!/usr/bin/env python3
"""Data-only candidate preflight helpers for upstream convergence."""

import argparse
import hashlib
import json
import re
import sys
import tomllib
import urllib.error
import urllib.request
from pathlib import Path


MAX_ERROR_LENGTH = 1000
MAX_EVIDENCE_REASON_LENGTH = 1000
MAX_CONFLICT_PATHS = 200
MAX_DOWNLOAD_BYTES = 1024 * 1024 * 1024
SHA256_RE = re.compile(r"[0-9a-f]{64}")
SHA1_RE = re.compile(r"[0-9a-f]{40}")
VERSION_RE = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")
REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
RELEASE_ROOT = "https://github.com/openai/codex/releases/download"
TARGET = "aarch64-apple-darwin"
PROFILE = "ptrcomp_sandbox_release"


class CandidatePreflightError(ValueError):
    def __init__(self, message: str, classification: str = "preflight-blocked") -> None:
        super().__init__(message)
        self.classification = classification


def bounded(value: object, limit: int) -> str:
    return str(value).replace("\r", " ").replace("\n", " ")[:limit]


def candidate_file(candidate_dir: Path, relative_path: str) -> Path:
    try:
        root = candidate_dir.resolve(strict=True)
        path = (root / relative_path).resolve(strict=True)
    except OSError as error:
        raise CandidatePreflightError(
            f"candidate path is unavailable: {relative_path}"
        ) from error
    try:
        path.relative_to(root)
    except ValueError as error:
        raise CandidatePreflightError(
            f"candidate path escapes worktree: {relative_path}"
        ) from error
    return path


def candidate_v8_package(candidate_dir: Path) -> tuple[str, str]:
    lock_path = candidate_file(candidate_dir, "codex-rs/Cargo.lock")
    try:
        lock_data = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise CandidatePreflightError(f"unable to read candidate Cargo.lock: {error}") from error
    packages = lock_data.get("package")
    if not isinstance(packages, list):
        raise CandidatePreflightError("candidate Cargo.lock has no package list")
    matches = [
        package
        for package in packages
        if isinstance(package, dict) and package.get("name") == "v8"
    ]
    if len(matches) != 1:
        raise CandidatePreflightError("candidate Cargo.lock must contain exactly one v8 package")
    package = matches[0]
    version = package.get("version")
    checksum = package.get("checksum")
    source = package.get("source")
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        raise CandidatePreflightError("candidate Cargo.lock has an invalid v8 version")
    if not isinstance(checksum, str) or not SHA256_RE.fullmatch(checksum):
        raise CandidatePreflightError("candidate Cargo.lock has an invalid v8 crate checksum")
    if source != REGISTRY_SOURCE:
        raise CandidatePreflightError("candidate Cargo.lock has an unexpected v8 source")
    return version, checksum


def release_checksums(candidate_dir: Path, version: str) -> dict[str, str]:
    manifest_path = candidate_file(
        candidate_dir,
        f"third_party/v8/rusty_v8_{version.replace('.', '_')}_codex_release.sha256",
    )
    try:
        lines = manifest_path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        raise CandidatePreflightError(
            f"unable to read candidate rusty_v8 checksum manifest: {error}"
        ) from error
    checksums = {}
    for line_number, line in enumerate(lines, 1):
        parts = line.split()
        if len(parts) != 2 or not SHA256_RE.fullmatch(parts[0]):
            raise CandidatePreflightError(
                f"invalid candidate checksum manifest line {line_number}"
            )
        checksum, filename = parts
        if "/" in filename or filename in {"", ".", ".."} or filename in checksums:
            raise CandidatePreflightError(
                f"invalid candidate checksum manifest filename at line {line_number}"
            )
        checksums[filename] = checksum
    if not checksums:
        raise CandidatePreflightError("candidate rusty_v8 checksum manifest is empty")
    return checksums


def verified_download(url: str, expected_checksum: str, destination: Path) -> int:
    digest = hashlib.sha256()
    size = 0
    try:
        with urllib.request.urlopen(url, timeout=120) as response, destination.open("wb") as output:
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                if size > MAX_DOWNLOAD_BYTES:
                    raise CandidatePreflightError(
                        f"release asset exceeds {MAX_DOWNLOAD_BYTES} bytes",
                        "infrastructure",
                    )
                digest.update(chunk)
                output.write(chunk)
    except (OSError, urllib.error.URLError) as error:
        raise CandidatePreflightError(
            f"unable to download release asset: {error}", "infrastructure"
        ) from error
    actual_checksum = digest.hexdigest()
    if actual_checksum != expected_checksum:
        raise CandidatePreflightError(
            f"release asset checksum mismatch: expected {expected_checksum}, got {actual_checksum}",
            "infrastructure",
        )
    return size


def run_preflight(candidate_dir: Path, download_dir: Path, output: Path) -> int:
    try:
        version, crate_checksum = candidate_v8_package(candidate_dir)
        checksums = release_checksums(candidate_dir, version)
        names = [
            f"librusty_v8_{PROFILE}_{TARGET}.a.gz",
            f"src_binding_{PROFILE}_{TARGET}.rs",
        ]
        download_dir.mkdir(parents=True, exist_ok=True)
        assets = []
        for name in names:
            checksum = checksums.get(name)
            if checksum is None:
                raise CandidatePreflightError(
                    f"candidate checksum manifest is missing {name}"
                )
            url = f"{RELEASE_ROOT}/rusty-v8-v{version}/{name}"
            size = verified_download(url, checksum, download_dir / name)
            assets.append({"name": name, "manifestSha256": checksum, "bytes": size})
        result = {
            "status": "passed",
            "v8Version": version,
            "v8CrateChecksum": crate_checksum,
            "assets": assets,
        }
        return_code = 0
    except CandidatePreflightError as error:
        result = {
            "status": "failed",
            "classification": error.classification,
            "error": bounded(error, MAX_ERROR_LENGTH),
        }
        return_code = 3 if error.classification == "preflight-blocked" else 1
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return return_code


def load_preflight(path: Path) -> dict[str, object] | None:
    if not path.exists():
        return None
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return {"status": "unavailable"}
    return value if isinstance(value, dict) else {"status": "unavailable"}


def read_conflict_paths(path: Path | None) -> list[str]:
    if path is None or not path.exists():
        return []
    try:
        paths = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        return []
    return [bounded(path, 512) for path in paths[:MAX_CONFLICT_PATHS]]


def write_evidence(args: argparse.Namespace) -> int:
    refs = {"base": args.base, "upstream": args.upstream, "local": args.local}
    if not all(SHA1_RE.fullmatch(value) for value in refs.values()):
        raise CandidatePreflightError("candidate evidence requires exact 40-hex refs")
    if not SHA1_RE.fullmatch(args.workflow_sha):
        raise CandidatePreflightError("candidate evidence requires an exact workflow SHA")
    conflicts = read_conflict_paths(args.conflicts)
    if args.conflict_total < len(conflicts):
        raise CandidatePreflightError("candidate conflict total is smaller than evidence")
    preflight = load_preflight(args.preflight)
    result = {
        "schemaVersion": 1,
        "classification": args.classification,
        "candidateDue": True,
        "gateStatus": bounded(args.gate_status, 64),
        "snapshot": bounded(args.snapshot, 512),
        "refs": refs,
        "reason": bounded(args.reason, MAX_EVIDENCE_REASON_LENGTH),
        "conflictPathTotal": args.conflict_total,
        "conflictPathsTruncated": args.conflict_total > len(conflicts),
        "conflictPaths": conflicts,
        "temporaryWorktreeRemoved": args.worktree_removed == "true",
        "primaryCheckoutClean": args.primary_checkout_clean == "true",
        "rustyV8Preflight": preflight,
        "workflow": {"sha": args.workflow_sha, "runId": bounded(args.run_id, 128)},
    }
    args.output_dir.mkdir(parents=True, exist_ok=True)
    json_path = args.output_dir / "candidate-evidence.json"
    text_path = args.output_dir / "candidate-evidence.txt"
    json_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    lines = [
        "Upstream convergence candidate stage 3a",
        f"classification: {result['classification']}",
        f"reason: {result['reason']}",
        f"refs: base={args.base} upstream={args.upstream} local={args.local}",
        f"conflicts: {result['conflictPathTotal']}",
        f"temporary worktree removed: {result['temporaryWorktreeRemoved']}",
        f"primary checkout clean: {result['primaryCheckoutClean']}",
        f"rusty_v8 preflight: {preflight.get('status') if preflight else 'not-run'}",
    ]
    text_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    preflight = commands.add_parser("preflight")
    preflight.add_argument("--candidate-dir", type=Path, required=True)
    preflight.add_argument("--download-dir", type=Path, required=True)
    preflight.add_argument("--output", type=Path, required=True)
    evidence = commands.add_parser("write-evidence")
    evidence.add_argument(
        "--classification",
        choices=("clean", "conflict", "infrastructure", "preflight-blocked"),
        required=True,
    )
    evidence.add_argument("--reason", required=True)
    evidence.add_argument("--base", required=True)
    evidence.add_argument("--upstream", required=True)
    evidence.add_argument("--local", required=True)
    evidence.add_argument("--gate-status", required=True)
    evidence.add_argument("--snapshot", required=True)
    evidence.add_argument("--conflicts", type=Path)
    evidence.add_argument("--conflict-total", type=int, default=0)
    evidence.add_argument("--preflight", type=Path)
    evidence.add_argument("--worktree-removed", choices=("true", "false"), required=True)
    evidence.add_argument("--primary-checkout-clean", choices=("true", "false"), required=True)
    evidence.add_argument("--output-dir", type=Path, required=True)
    evidence.add_argument("--workflow-sha", required=True)
    evidence.add_argument("--run-id", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.command == "preflight":
        return run_preflight(args.candidate_dir, args.download_dir, args.output)
    try:
        return write_evidence(args)
    except CandidatePreflightError as error:
        print(bounded(error, MAX_ERROR_LENGTH), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
