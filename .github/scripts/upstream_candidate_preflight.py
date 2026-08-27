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
MAX_AFFECTED_CONTRACTS = 50
MAX_AFFECTED_PATHS = 100
MAX_AFFECTED_TIERS = 8
MAX_SUGGESTED_TESTS = 25
MAX_LOG_LINES = 200
MAX_LOG_BYTES = 64 * 1024
MAX_JSON_BYTES = 512 * 1024
MAX_CHANGED_PATH_BYTES = 1024 * 1024
SHA256_RE = re.compile(r"[0-9a-f]{64}")
SHA1_RE = re.compile(r"[0-9a-f]{40}")
VERSION_RE = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")
PACKAGE_RE = re.compile(r"[A-Za-z0-9_.-]+")
REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
RELEASE_ROOT = "https://github.com/openai/codex/releases/download"
TARGET = "aarch64-apple-darwin"
PROFILE = "ptrcomp_sandbox_release"
EXCLUDED_EVIDENCE_KINDS = frozenset({"narrative", "semantic_reachability"})


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


def read_path_list(path: Path) -> list[str]:
    try:
        if path.stat().st_size > MAX_CHANGED_PATH_BYTES:
            raise CandidatePreflightError("changed-path input exceeds the bounded size", "infrastructure")
        values = path.read_bytes().split(b"\0")
        if values and not values[-1]:
            values.pop()
        values = [value.decode("utf-8") for value in values]
    except (OSError, UnicodeError) as error:
        raise CandidatePreflightError(f"unable to read changed paths: {error}", "infrastructure") from error
    if any(not value or len(value) > 512 for value in values):
        raise CandidatePreflightError("changed-path input contains an invalid path", "infrastructure")
    return sorted(set(values))


def _read_json_object(path: Path, label: str) -> dict[str, object]:
    try:
        if path.stat().st_size > MAX_JSON_BYTES:
            raise CandidatePreflightError(f"{label} exceeds the bounded JSON size", "infrastructure")
        value = json.loads(path.read_text(encoding="utf-8"))
    except CandidatePreflightError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise CandidatePreflightError(f"unable to read {label}: {error}", "infrastructure") from error
    if not isinstance(value, dict):
        raise CandidatePreflightError(f"{label} must be a JSON object", "infrastructure")
    return value


def _bounded_stream(stream: object) -> str:
    data = bytearray()
    while chunk := stream.read(MAX_LOG_BYTES):
        data.extend(chunk)
        if len(data) > MAX_LOG_BYTES:
            del data[:-MAX_LOG_BYTES]
    lines = bytes(data).splitlines(keepends=True)[-MAX_LOG_LINES:]
    text = b"".join(lines).decode("utf-8", errors="replace")
    return "".join(f" {line}" if line.startswith("::") else line for line in text.splitlines(keepends=True))


def bounded_log(path: Path) -> str:
    try:
        with path.open("rb") as input_file:
            return _bounded_stream(input_file)
    except OSError as error:
        raise CandidatePreflightError(f"unable to read command log: {error}", "infrastructure") from error


def bounded_log_from_stdin() -> str:
    return _bounded_stream(sys.stdin.buffer)


def classify_failure(status: int, log: str) -> str:
    if status == 0:
        return "passed"
    if status in {126, 127, 137}:
        return "infrastructure"
    lowered = log.casefold()
    if any(
        marker in lowered
        for marker in (
            "no space left on device",
            "disk full",
            "runner lost",
            "runner has received",
            "cannot allocate memory",
            "could not resolve host",
            "network is unreachable",
            "failed to download",
            "failed to fetch",
            "connection reset",
            "error sending request",
            "spurious network error",
            "network failure seems to have happened",
            "operation timed out",
            "resource temporarily unavailable",
            "too many open files",
            "operation not permitted",
            "permission denied",
            "read-only file system",
        )
    ):
        return "infrastructure"
    return "regression"


def _package_for_path(candidate_dir: Path, relative_path: str) -> str | None:
    path = Path(relative_path)
    if not path.parts or path.parts[0] != "codex-rs":
        return None
    current = (candidate_dir / path).resolve(strict=False).parent
    root = (candidate_dir / "codex-rs").resolve(strict=False)
    while current == root or root in current.parents:
        cargo_toml = current / "Cargo.toml"
        if cargo_toml.is_file():
            try:
                package = tomllib.loads(cargo_toml.read_text(encoding="utf-8")).get("package")
            except (OSError, UnicodeError, tomllib.TOMLDecodeError):
                return None
            name = package.get("name") if isinstance(package, dict) else None
            return name if isinstance(name, str) and PACKAGE_RE.fullmatch(name) else None
        if current == root:
            break
        current = current.parent
    return None


def select_affected_contracts(
    gates_path: Path,
    upstream_paths_path: Path,
    local_paths_path: Path,
    candidate_dir: Path,
    output: Path,
) -> int:
    try:
        gates = _read_json_object(gates_path, "convergence gates")
        upstream_paths = set(read_path_list(upstream_paths_path))
        local_paths = set(read_path_list(local_paths_path))
        overlap = sorted(upstream_paths & local_paths)
        contracts = gates.get("contracts")
        if not isinstance(contracts, list):
            raise CandidatePreflightError("convergence gates has no contract list", "infrastructure")
        selected = []
        suggested_tests: set[str] = set()
        for contract in contracts:
            if not isinstance(contract, dict):
                raise CandidatePreflightError("convergence gate contract is not an object", "infrastructure")
            contract_id = contract.get("id")
            evidence = contract.get("evidence")
            if not isinstance(contract_id, str) or not contract_id or len(contract_id) > 128:
                raise CandidatePreflightError("convergence gate contract has an invalid id", "infrastructure")
            if not isinstance(evidence, list):
                raise CandidatePreflightError(f"contract {contract_id} has invalid evidence", "infrastructure")
            matches = []
            tiers = set()
            packages = set()
            for item in evidence:
                if not isinstance(item, dict):
                    raise CandidatePreflightError(f"contract {contract_id} has invalid evidence", "infrastructure")
                if item.get("kind") in EXCLUDED_EVIDENCE_KINDS:
                    continue
                path = item.get("path")
                if not isinstance(path, str) or path not in overlap:
                    continue
                matches.append(path)
                tier = item.get("ciTier")
                if isinstance(tier, str) and tier:
                    tiers.add(bounded(tier, 64))
                package = _package_for_path(candidate_dir, path)
                if package:
                    packages.add(package)
            if matches:
                unique_matches = sorted(set(matches))
                commands = [f"just test -p {package}" for package in sorted(packages)]
                suggested_tests.update(commands)
                selected.append(
                    {
                        "id": contract_id,
                        "matchedPathTotal": len(unique_matches),
                        "matchedPaths": unique_matches[:MAX_AFFECTED_PATHS],
                        "ciTiers": sorted(tiers)[:MAX_AFFECTED_TIERS],
                        "suggestedTests": commands[:MAX_SUGGESTED_TESTS],
                    }
                )
        selected.sort(key=lambda contract: contract["id"])
        suggested_tests = sorted(suggested_tests)
        truncated = (
            len(selected) > MAX_AFFECTED_CONTRACTS
            or len(overlap) > MAX_AFFECTED_PATHS
            or len(suggested_tests) > MAX_SUGGESTED_TESTS
            or any(contract["matchedPathTotal"] > MAX_AFFECTED_PATHS for contract in selected)
        )
        result = {
            "status": "passed",
            "overlapPathTotal": len(upstream_paths & local_paths),
            "overlapPaths": overlap[:MAX_AFFECTED_PATHS],
            "contractTotal": len(selected),
            "contracts": selected[:MAX_AFFECTED_CONTRACTS],
            "suggestedTestTotal": len(suggested_tests),
            "suggestedTests": suggested_tests[:MAX_SUGGESTED_TESTS],
            "truncated": truncated,
        }
        output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        return 0
    except CandidatePreflightError as error:
        output.write_text(
            json.dumps(
                {"status": "failed", "classification": error.classification, "error": bounded(error, MAX_ERROR_LENGTH)},
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        return 1


def record_stage3b(args: argparse.Namespace) -> int:
    try:
        evidence = _read_json_object(args.evidence, "candidate evidence")
        repo_checks = _read_json_object(args.repo_checks, "repository-check outcome")
        cargo_check = _read_json_object(args.cargo_check, "cargo outcome")
        affected = _read_json_object(args.affected_contracts, "affected-contract outcome")
        root_failures = _read_json_object(args.root_failures, "root-failure outcome")
        evidence["repoChecks"] = repo_checks
        evidence["cargoCheck"] = cargo_check
        evidence["affectedContracts"] = affected
        evidence["rootFailures"] = root_failures
        outcomes = (repo_checks, cargo_check, affected, root_failures)
        classifications = {outcome.get("classification") for outcome in outcomes}
        if "infrastructure" in classifications:
            evidence["classification"] = "infrastructure"
        elif "regression" in classifications:
            evidence["classification"] = "regression"
        elif any(outcome.get("status") == "not-run" for outcome in outcomes):
            evidence["classification"] = "infrastructure"
        else:
            evidence["classification"] = "clean"
        args.evidence.write_text(json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        text_path = args.evidence.with_name("candidate-evidence.txt")
        prefixes = ("classification:", "reason:", "repo checks:", "cargo check:", "affected contracts:", "root failures:", "temporary worktree removed:", "primary checkout clean:")
        existing = text_path.read_text(encoding="utf-8") if text_path.exists() else ""
        lines = [line for line in existing.splitlines() if not line.startswith(prefixes)]
        lines.extend([
            f"classification: {evidence.get('classification')}", f"reason: {evidence.get('reason', '')}",
            f"repo checks: {repo_checks.get('status')}", f"cargo check: {cargo_check.get('status')}",
            f"affected contracts: {affected.get('contractTotal', 0)}", f"root failures: {root_failures.get('status')}",
            f"temporary worktree removed: {evidence.get('temporaryWorktreeRemoved')}", f"primary checkout clean: {evidence.get('primaryCheckoutClean')}",
        ])
        text_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return 0
    except CandidatePreflightError as error:
        print(bounded(error, MAX_ERROR_LENGTH), file=sys.stderr)
        return 1


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
        "repoChecks": {"status": "not-run"},
        "cargoCheck": {"status": "not-run", "command": "cargo check --workspace --tests --locked"},
        "affectedContracts": {"status": "not-run"},
        "rootFailures": {"status": "not-run"},
        "workflow": {"sha": args.workflow_sha, "runId": bounded(args.run_id, 128)},
    }
    args.output_dir.mkdir(parents=True, exist_ok=True)
    json_path = args.output_dir / "candidate-evidence.json"
    text_path = args.output_dir / "candidate-evidence.txt"
    json_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    lines = [
        "Upstream convergence candidate stages 3a and 3b",
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
        choices=("clean", "conflict", "infrastructure", "preflight-blocked", "validation-pending"),
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
    affected = commands.add_parser("select-affected-contracts")
    affected.add_argument("--gates", type=Path, required=True)
    affected.add_argument("--upstream-paths", type=Path, required=True)
    affected.add_argument("--local-paths", type=Path, required=True)
    affected.add_argument("--candidate-dir", type=Path, required=True)
    affected.add_argument("--output", type=Path, required=True)
    stage3b = commands.add_parser("record-stage3b")
    stage3b.add_argument("--evidence", type=Path, required=True)
    stage3b.add_argument("--repo-checks", type=Path, required=True)
    stage3b.add_argument("--cargo-check", type=Path, required=True)
    stage3b.add_argument("--affected-contracts", type=Path, required=True)
    stage3b.add_argument("--root-failures", type=Path, required=True)
    log = commands.add_parser("bound-log")
    log.add_argument("--output", type=Path, required=True)
    classify = commands.add_parser("classify-failure")
    classify.add_argument("--status", type=int, required=True)
    classify.add_argument("--log", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.command == "preflight":
        return run_preflight(args.candidate_dir, args.download_dir, args.output)
    if args.command == "select-affected-contracts":
        return select_affected_contracts(
            args.gates,
            args.upstream_paths,
            args.local_paths,
            args.candidate_dir,
            args.output,
        )
    if args.command == "bound-log":
        args.output.write_text(bounded_log_from_stdin(), encoding="utf-8")
        return 0
    if args.command == "classify-failure":
        print(classify_failure(args.status, bounded_log(args.log)))
        return 0
    if args.command == "record-stage3b":
        return record_stage3b(args)
    try:
        return write_evidence(args)
    except CandidatePreflightError as error:
        print(bounded(error, MAX_ERROR_LENGTH), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
