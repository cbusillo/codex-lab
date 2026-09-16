#!/usr/bin/env python3
"""Data-only candidate preflight helpers for upstream convergence."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
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
MAX_PACKET_BYTES = 40_000
MAX_PACKET_TOKENS = 10_000
MAX_AGGREGATE_PACKET_TOKENS = 40_000
MAX_PACKETS = 12
MAX_PACKET_PATHS = 25
MAX_PACKET_ANCHORS = 20
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


class TrustedPacketInputError(ValueError):
    pass


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
        raise CandidatePreflightError(
            f"unable to read candidate Cargo.lock: {error}"
        ) from error
    packages = lock_data.get("package")
    if not isinstance(packages, list):
        raise CandidatePreflightError("candidate Cargo.lock has no package list")
    matches = [
        package
        for package in packages
        if isinstance(package, dict) and package.get("name") == "v8"
    ]
    if len(matches) != 1:
        raise CandidatePreflightError(
            "candidate Cargo.lock must contain exactly one v8 package"
        )
    package = matches[0]
    version = package.get("version")
    checksum = package.get("checksum")
    source = package.get("source")
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        raise CandidatePreflightError("candidate Cargo.lock has an invalid v8 version")
    if not isinstance(checksum, str) or not SHA256_RE.fullmatch(checksum):
        raise CandidatePreflightError(
            "candidate Cargo.lock has an invalid v8 crate checksum"
        )
    if source != REGISTRY_SOURCE:
        raise CandidatePreflightError(
            "candidate Cargo.lock has an unexpected v8 source"
        )
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
        with (
            urllib.request.urlopen(url, timeout=120) as response,
            destination.open("wb") as output,
        ):
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
    output.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
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
            raise CandidatePreflightError(
                "changed-path input exceeds the bounded size", "infrastructure"
            )
        values = path.read_bytes().split(b"\0")
        if values and not values[-1]:
            values.pop()
        values = [value.decode("utf-8") for value in values]
    except (OSError, UnicodeError) as error:
        raise CandidatePreflightError(
            f"unable to read changed paths: {error}", "infrastructure"
        ) from error
    if any(not value or len(value) > 512 for value in values):
        raise CandidatePreflightError(
            "changed-path input contains an invalid path", "infrastructure"
        )
    return sorted(set(values))


def _read_json_object(path: Path, label: str) -> dict[str, object]:
    try:
        if path.stat().st_size > MAX_JSON_BYTES:
            raise CandidatePreflightError(
                f"{label} exceeds the bounded JSON size", "infrastructure"
            )
        value = json.loads(path.read_text(encoding="utf-8"))
    except CandidatePreflightError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise CandidatePreflightError(
            f"unable to read {label}: {error}", "infrastructure"
        ) from error
    if not isinstance(value, dict):
        raise CandidatePreflightError(
            f"{label} must be a JSON object", "infrastructure"
        )
    return value


def _bounded_stream(stream: object) -> str:
    data = bytearray()
    while chunk := stream.read(MAX_LOG_BYTES):
        data.extend(chunk)
        if len(data) > MAX_LOG_BYTES:
            del data[:-MAX_LOG_BYTES]
    lines = bytes(data).splitlines(keepends=True)[-MAX_LOG_LINES:]
    text = b"".join(lines).decode("utf-8", errors="replace")
    return "".join(
        f" {line}" if line.startswith("::") else line
        for line in text.splitlines(keepends=True)
    )


def bounded_log(path: Path) -> str:
    try:
        with path.open("rb") as input_file:
            return _bounded_stream(input_file)
    except OSError as error:
        raise CandidatePreflightError(
            f"unable to read command log: {error}", "infrastructure"
        ) from error


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
                package = tomllib.loads(cargo_toml.read_text(encoding="utf-8")).get(
                    "package"
                )
            except (OSError, UnicodeError, tomllib.TOMLDecodeError):
                return None
            name = package.get("name") if isinstance(package, dict) else None
            return (
                name if isinstance(name, str) and PACKAGE_RE.fullmatch(name) else None
            )
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
            raise CandidatePreflightError(
                "convergence gates has no contract list", "infrastructure"
            )
        selected = []
        suggested_tests: set[str] = set()
        for contract in contracts:
            if not isinstance(contract, dict):
                raise CandidatePreflightError(
                    "convergence gate contract is not an object", "infrastructure"
                )
            contract_id = contract.get("id")
            evidence = contract.get("evidence")
            if (
                not isinstance(contract_id, str)
                or not contract_id
                or len(contract_id) > 128
            ):
                raise CandidatePreflightError(
                    "convergence gate contract has an invalid id", "infrastructure"
                )
            if not isinstance(evidence, list):
                raise CandidatePreflightError(
                    f"contract {contract_id} has invalid evidence", "infrastructure"
                )
            matches = []
            tiers = set()
            packages = set()
            for item in evidence:
                if not isinstance(item, dict):
                    raise CandidatePreflightError(
                        f"contract {contract_id} has invalid evidence", "infrastructure"
                    )
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
            or any(
                contract["matchedPathTotal"] > MAX_AFFECTED_PATHS
                for contract in selected
            )
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
        output.write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        return 0
    except CandidatePreflightError as error:
        output.write_text(
            json.dumps(
                {
                    "status": "failed",
                    "classification": error.classification,
                    "error": bounded(error, MAX_ERROR_LENGTH),
                },
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
        affected = _read_json_object(
            args.affected_contracts, "affected-contract outcome"
        )
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
        args.evidence.write_text(
            json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        text_path = args.evidence.with_name("candidate-evidence.txt")
        prefixes = (
            "classification:",
            "reason:",
            "repo checks:",
            "cargo check:",
            "affected contracts:",
            "root failures:",
            "temporary worktree removed:",
            "primary checkout clean:",
        )
        existing = text_path.read_text(encoding="utf-8") if text_path.exists() else ""
        lines = [
            line for line in existing.splitlines() if not line.startswith(prefixes)
        ]
        lines.extend(
            [
                f"classification: {evidence.get('classification')}",
                f"reason: {evidence.get('reason', '')}",
                f"repo checks: {repo_checks.get('status')}",
                f"cargo check: {cargo_check.get('status')}",
                f"affected contracts: {affected.get('contractTotal', 0)}",
                f"root failures: {root_failures.get('status')}",
                f"temporary worktree removed: {evidence.get('temporaryWorktreeRemoved')}",
                f"primary checkout clean: {evidence.get('primaryCheckoutClean')}",
            ]
        )
        text_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return 0
    except CandidatePreflightError as error:
        print(bounded(error, MAX_ERROR_LENGTH), file=sys.stderr)
        return 1


def _canonical_json(value: object) -> bytes:
    return json.dumps(
        value, ensure_ascii=True, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")


def _packet_tokens(packet: dict[str, object]) -> int:
    return math.ceil(len(_canonical_json(packet)) / 4)


def _safe_evidence(item: dict[str, object]) -> dict[str, object]:
    result = {"kind": bounded(item["kind"], 64)}
    for key in ("path", "ciTier", "token", "description"):
        value = item.get(key)
        if isinstance(value, str):
            result[key] = bounded(value, 512)
    return result


def _safe_guard(entry: dict[str, object] | None) -> dict[str, object]:
    if entry is None:
        return {"lane": "unattributed"}
    result = {"lane": bounded(entry["lane"], 64)}
    for key in ("reason", "source"):
        value = entry.get(key)
        if isinstance(value, str):
            result[key] = bounded(value, 512)
    contracts = entry.get("contracts", [])
    result["contracts"] = sorted(
        bounded(contract, 128) for contract in contracts if isinstance(contract, str)
    )[:MAX_AFFECTED_CONTRACTS]
    return result


def _fit_packet(
    packet: dict[str, object],
    paths: list[dict[str, object]],
    anchors: list[dict[str, object]],
) -> tuple[dict[str, object], int, int]:
    packet["paths"] = paths[:MAX_PACKET_PATHS]
    packet["anchors"] = anchors[:MAX_PACKET_ANCHORS]
    deferred_reasons = []
    if len(paths) > MAX_PACKET_PATHS:
        deferred_reasons.append("path_count_cap")
    if len(anchors) > MAX_PACKET_ANCHORS:
        deferred_reasons.append("anchor_count_cap")
    while True:
        packet["deferredReasons"] = sorted(set(deferred_reasons))
        packet["includedPathTotal"] = len(packet["paths"])
        packet["includedAnchorTotal"] = len(packet["anchors"])
        packet["deferredPathTotal"] = packet["pathTotal"] - packet["includedPathTotal"]
        packet["deferredAnchorTotal"] = (
            packet["anchorTotal"] - packet["includedAnchorTotal"]
        )
        packet["estimatedPromptTokens"] = 0
        for _ in range(4):
            estimated_tokens = _packet_tokens(packet)
            if packet["estimatedPromptTokens"] == estimated_tokens:
                break
            packet["estimatedPromptTokens"] = estimated_tokens
        if len(_canonical_json(packet)) <= MAX_PACKET_BYTES:
            break
        if packet["anchors"]:
            packet["anchors"].pop()
            deferred_reasons.append("packet_byte_cap")
        elif packet["paths"]:
            packet["paths"].pop()
            deferred_reasons.append("packet_byte_cap")
        else:
            raise ValueError("packet metadata exceeds the hard byte cap")
    if packet["estimatedPromptTokens"] > MAX_PACKET_TOKENS:
        raise ValueError("packet metadata exceeds the hard token cap")
    return packet, packet["deferredPathTotal"], packet["deferredAnchorTotal"]


def _packet_from_contract(
    contract_id: str,
    evidence: list[dict[str, object]],
    matched: set[str],
    guard_by_path: dict[str, dict[str, object]],
    attribution: dict[str, set[str]],
) -> tuple[dict[str, object], int, int]:
    matched = sorted(matched)
    path_items = []
    for path in matched:
        guard = guard_by_path.get(path)
        path_items.append(
            {
                "path": path,
                "guard": _safe_guard(guard),
                "attributedContracts": sorted(attribution.get(path, set())),
            }
        )
    anchors = [
        _safe_evidence(item)
        for item in evidence
        if item.get("kind") not in EXCLUDED_EVIDENCE_KINDS
    ]
    anchors.sort(
        key=lambda item: (
            item.get("kind", ""),
            item.get("path", ""),
            item.get("token", ""),
        )
    )
    lanes = sorted(
        {
            guard_by_path[path].get("lane")
            for path in matched
            if path in guard_by_path
            and isinstance(guard_by_path[path].get("lane"), str)
        }
    )
    reasons = []
    if "red_manual_review" in lanes:
        reasons.append("red_manual_review_lane")
    if any(len(attribution.get(path, set())) > 1 for path in matched):
        reasons.append("ambiguous_contract_attribution")
    excluded = sum(
        1 for item in evidence if item.get("kind") in EXCLUDED_EVIDENCE_KINDS
    )
    packet = {
        "schemaVersion": 1,
        "packetId": f"contract:{contract_id}",
        "kind": "contract",
        "subject": contract_id,
        "modelTier": "frontier" if reasons else "budget",
        "escalationReasons": sorted(reasons),
        "pathTotal": len(matched),
        "anchorTotal": len(anchors),
        "excludedAnchorTotal": excluded,
        "guardLanes": lanes,
    }
    return _fit_packet(packet, path_items, anchors)


def _root_packets(root_failures: dict[str, object] | None) -> list[dict[str, object]]:
    if not root_failures or root_failures.get("status") != "extracted":
        return []
    report = root_failures.get("report")
    failures = report.get("failures") if isinstance(report, dict) else None
    if not isinstance(failures, list):
        raise ValueError("root-failure outcome has no bounded failures")
    packets = []
    for index, failure in enumerate(failures):
        if not isinstance(failure, dict) or not isinstance(failure.get("root"), str):
            raise ValueError("root-failure outcome has an invalid failure")
        sources = failure.get("sources", [])
        if not isinstance(sources, list):
            raise ValueError("root-failure outcome has invalid sources")
        safe_sources = []
        for source in sources[:8]:
            if not isinstance(source, dict):
                raise ValueError("root-failure outcome has an invalid source")
            safe_source = {}
            for key in ("conclusion", "id", "kind", "name"):
                value = source.get(key)
                if isinstance(value, str):
                    safe_source[key] = bounded(value, 256)
                elif (
                    key == "id"
                    and isinstance(value, int)
                    and not isinstance(value, bool)
                ):
                    safe_source[key] = value
            safe_sources.append(safe_source)
        root = bounded(failure["root"], 512)
        packet = {
            "schemaVersion": 1,
            "packetId": f"root:{index:02d}:{root}",
            "kind": "root_failure",
            "subject": root,
            "modelTier": "budget",
            "escalationReasons": [],
            "pathTotal": 0,
            "anchorTotal": 0,
            "excludedAnchorTotal": 0,
            "sourceTotal": len(safe_sources),
            "rootFailure": {"root": root, "sources": safe_sources},
        }
        packets.append(_fit_packet(packet, [], [])[0])
    return packets


def _write_packet_artifacts(
    output_dir: Path,
    packets_result: dict[str, object],
    telemetry: dict[str, object],
    evidence_path: Path,
) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "model-packets.json").write_text(
        json.dumps(packets_result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    lines = [
        "Upstream convergence model packets (stage 3c)",
        f"status: {packets_result['status']}",
        f"outcome: {packets_result['outcome']}",
        f"packets: {packets_result['plannedPacketTotal']} of {packets_result['packetTotal']}",
        f"planned prompt tokens: {packets_result['aggregatePlannedPromptTokens']}",
        f"mechanical or unattributed paths: {packets_result['counts']['mechanicalOrUnattributedPathTotal']}",
        f"excluded anchors: {packets_result['counts']['excludedAnchorTotal']}",
        f"invocation: {telemetry['invocation']}",
    ]
    lines.extend(f"warning: {warning}" for warning in packets_result["warnings"])
    (output_dir / "model-packets.txt").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )
    (output_dir / "model-telemetry.json").write_text(
        json.dumps(telemetry, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    if evidence_path.exists():
        try:
            evidence = _read_json_object(evidence_path, "candidate evidence")
        except CandidatePreflightError:
            evidence = None
        if evidence is not None:
            evidence["modelPackets"] = {
                "status": packets_result["status"],
                "packetTotal": packets_result["packetTotal"],
                "plannedPacketTotal": packets_result["plannedPacketTotal"],
                "deferredPacketTotal": packets_result["deferredPacketTotal"],
                "aggregatePlannedPromptTokens": packets_result[
                    "aggregatePlannedPromptTokens"
                ],
            }
            evidence["modelTelemetry"] = {
                "status": telemetry["status"],
                "invocation": telemetry["invocation"],
                "calls": 0,
                "promptTokens": 0,
                "completionTokens": 0,
                "totalTokens": 0,
                "actualPromptTokens": 0,
                "actualCompletionTokens": 0,
                "actualTotalTokens": 0,
                "plannedModelTier": telemetry["plannedModelTier"],
                "outcome": telemetry["outcome"],
            }
            evidence_path.write_text(
                json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
            text_path = evidence_path.with_name("candidate-evidence.txt")
            existing = (
                text_path.read_text(encoding="utf-8") if text_path.exists() else ""
            )
            prefixes = ("model packets:", "model telemetry:")
            lines = [
                line for line in existing.splitlines() if not line.startswith(prefixes)
            ]
            lines.extend(
                [
                    f"model packets: {packets_result['plannedPacketTotal']} of {packets_result['packetTotal']}",
                    f"model telemetry: {telemetry['outcome']} ({telemetry['invocation']})",
                ]
            )
            text_path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def build_model_packets(args: argparse.Namespace) -> int:
    try:
        args.output_dir.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        print(
            f"unable to materialize packet output directory: {error}", file=sys.stderr
        )
        return 1
    started_at = bounded(args.started_at, 128)
    started_time = None
    duration_ms = 0
    trusted_failure = False
    try:
        duration_ms = int(args.duration_ms)
        if duration_ms < 0:
            raise ValueError("packet-build duration cannot be negative")
        if duration_ms == 0:
            started_time = datetime.fromisoformat(started_at.replace("Z", "+00:00"))
            if started_time.tzinfo is None:
                raise ValueError("packet-build started-at must be timezone-aware")
        evidence = _read_json_object(args.evidence, "candidate evidence")
        try:
            guard = _read_json_object(args.guard, "convergence guard")
            gates = _read_json_object(args.gates, "convergence gates")
        except CandidatePreflightError as error:
            raise TrustedPacketInputError(str(error)) from error
        root_failures = (
            _read_json_object(args.root_failures, "root-failure outcome")
            if args.root_failures and args.root_failures.exists()
            else None
        )
        conflict_paths = read_conflict_paths(args.conflicts)
        if not conflict_paths:
            fallback = evidence.get("conflictPaths", [])
            conflict_paths = [
                bounded(path, 512) for path in fallback if isinstance(path, str)
            ]
        conflicts = set(conflict_paths)
        guarded = guard.get("guardedPaths")
        contracts = gates.get("contracts")
        if not isinstance(guarded, list) or not isinstance(contracts, list):
            raise TrustedPacketInputError(
                "trusted guard or gates schema is missing required lists"
            )
        attribution: dict[str, set[str]] = {}
        guard_by_path = {}
        for entry in guarded:
            if not isinstance(entry, dict) or not isinstance(entry.get("path"), str):
                raise TrustedPacketInputError("trusted guard has an invalid path entry")
            if not isinstance(entry.get("lane"), str) or not isinstance(
                entry.get("contracts"), list
            ):
                raise TrustedPacketInputError(
                    "trusted guard has an invalid contract lane"
                )
            if any(not isinstance(contract, str) for contract in entry["contracts"]):
                raise TrustedPacketInputError(
                    "trusted guard has an invalid contract id"
                )
            if entry["path"] in guard_by_path:
                raise TrustedPacketInputError(
                    "trusted guard has duplicate path entries"
                )
            guard_by_path[entry["path"]] = entry
            if entry["path"] in conflicts and entry["contracts"]:
                attribution.setdefault(entry["path"], set()).update(
                    bounded(contract, 128) for contract in entry["contracts"]
                )
        contract_data = {}
        for contract in contracts:
            if not isinstance(contract, dict) or not isinstance(
                contract.get("id"), str
            ):
                raise TrustedPacketInputError("trusted gates has an invalid contract")
            items = contract.get("evidence")
            if not isinstance(items, list):
                raise TrustedPacketInputError(
                    "trusted gates has invalid contract evidence"
                )
            contract_id = bounded(contract["id"], 128)
            if contract_id in contract_data:
                raise TrustedPacketInputError(
                    "trusted gates has duplicate contract ids"
                )
            contract_data[contract_id] = items
            for item in items:
                if not isinstance(item, dict) or not isinstance(item.get("kind"), str):
                    raise TrustedPacketInputError("trusted gates has invalid evidence")
                path = item.get("path")
                if path is not None and not isinstance(path, str):
                    raise TrustedPacketInputError(
                        "trusted gates has an invalid evidence path"
                    )
                if item["kind"] in EXCLUDED_EVIDENCE_KINDS:
                    continue
                if path in conflicts:
                    attribution.setdefault(path, set()).add(contract_id)
        packets = []
        attributed_contracts = {
            contract for values in attribution.values() for contract in values
        }
        for contract_id in sorted(set(contract_data) | attributed_contracts):
            matched = {
                path for path, values in attribution.items() if contract_id in values
            }
            if matched:
                packets.append(
                    _packet_from_contract(
                        contract_id,
                        contract_data.get(contract_id, []),
                        matched,
                        guard_by_path,
                        attribution,
                    )[0]
                )
        packets.extend(_root_packets(root_failures))
        excluded_anchor_total = sum(
            packet["excludedAnchorTotal"]
            for packet in packets
            if packet["kind"] == "contract"
        )
        packets.sort(
            key=lambda packet: (packet["modelTier"] != "frontier", packet["packetId"])
        )
        unattributed = conflicts - set(attribution)
        mechanical = sorted(path for path in unattributed if path not in guard_by_path)
        guarded_unattributed = sorted(unattributed & set(guard_by_path))
        warnings = []
        if unattributed:
            warnings.append(f"mechanical_or_unattributed_paths:{len(unattributed)}")
        if guarded_unattributed:
            warnings.append(f"guarded_unattributed_paths:{len(guarded_unattributed)}")
        root_status = (
            "missing"
            if root_failures is None
            else bounded(root_failures.get("status", "unknown"), 64)
        )
        if root_status != "extracted":
            warnings.append(f"root_failures_unavailable:{root_status}")
        if excluded_anchor_total:
            warnings.append(
                f"excluded_narrative_or_semantic_anchors:{excluded_anchor_total}"
            )
        if any(packet["modelTier"] == "frontier" for packet in packets):
            warnings.append("frontier_escalation_requires_named_reason")
        deferred_packets = []
        candidates = packets[:MAX_PACKETS]
        for packet in packets[MAX_PACKETS:]:
            deferred_packets.append(
                {
                    "packetId": packet["packetId"],
                    "reason": "packet_count_cap",
                    "estimatedPromptTokens": packet["estimatedPromptTokens"],
                }
            )
        planned = []
        aggregate = 0
        for packet in candidates:
            estimate = packet["estimatedPromptTokens"]
            if aggregate + estimate <= MAX_AGGREGATE_PACKET_TOKENS:
                planned.append(packet)
                aggregate += estimate
            else:
                deferred_packets.append(
                    {
                        "packetId": packet["packetId"],
                        "reason": "aggregate_prompt_token_cap",
                        "estimatedPromptTokens": estimate,
                    }
                )
        truncated_path_total = sum(packet["deferredPathTotal"] for packet in packets)
        if deferred_packets:
            warnings.append(f"packets_deferred:{len(deferred_packets)}")
        if truncated_path_total:
            warnings.append(f"packet_paths_truncated:{truncated_path_total}")
        planned.sort(key=lambda packet: packet["packetId"])
        deferred_packets.sort(key=lambda packet: packet["packetId"])
        outcome = (
            "packets-deferred"
            if deferred_packets
            else ("packets-built" if planned else "no-exception")
        )
        result = {
            "schemaVersion": 1,
            "stage": "3c",
            "cycleId": bounded(args.cycle_id, 128),
            "status": "built" if planned else "none-required",
            "outcome": outcome,
            "packets": planned,
            "packetTotal": len(packets),
            "plannedPacketTotal": len(planned),
            "deferredPacketTotal": len(deferred_packets),
            "deferredPackets": deferred_packets,
            "aggregatePlannedPromptTokens": aggregate,
            "aggregateTargetTokens": MAX_AGGREGATE_PACKET_TOKENS,
            "counts": {
                "conflictPathTotal": len(conflicts),
                "attributedPathTotal": len(attribution),
                "mechanicalOrUnattributedPathTotal": len(unattributed),
                "mechanicalPathTotal": len(mechanical),
                "guardedUnattributedPathTotal": len(guarded_unattributed),
                "excludedAnchorTotal": excluded_anchor_total,
            },
            "warnings": sorted(warnings),
            "trustedInputSchemas": {
                "guard": guard.get("schemaVersion"),
                "gates": gates.get("schemaVersion"),
            },
        }
    except (CandidatePreflightError, ValueError, TypeError, KeyError) as error:
        trusted_failure = isinstance(error, TrustedPacketInputError)
        result = {
            "schemaVersion": 1,
            "stage": "3c",
            "cycleId": bounded(args.cycle_id, 128),
            "status": "unavailable",
            "outcome": "unavailable",
            "packets": [],
            "packetTotal": 0,
            "plannedPacketTotal": 0,
            "deferredPacketTotal": 0,
            "deferredPackets": [],
            "aggregatePlannedPromptTokens": 0,
            "aggregateTargetTokens": MAX_AGGREGATE_PACKET_TOKENS,
            "counts": {
                "conflictPathTotal": 0,
                "attributedPathTotal": 0,
                "mechanicalOrUnattributedPathTotal": 0,
                "mechanicalPathTotal": 0,
                "guardedUnattributedPathTotal": 0,
                "excludedAnchorTotal": 0,
            },
            "warnings": [
                f"packet_build_unavailable:{bounded(error, MAX_ERROR_LENGTH)}"
            ],
            "trustedInputSchemas": {},
        }
        tiers = []
    else:
        tiers = sorted({packet["modelTier"] for packet in result["packets"]})
    finished_time = datetime.now(timezone.utc)
    finished_at = finished_time.isoformat().replace("+00:00", "Z")
    if started_time is not None:
        duration_ms = max(0, int((finished_time - started_time).total_seconds() * 1000))
    planned_tier = (
        "frontier"
        if tiers == ["frontier"]
        else (
            "budget"
            if tiers == ["budget"]
            else ("frontier-and-budget" if tiers else "none")
        )
    )
    telemetry = {
        "schemaVersion": 1,
        "stage": "3c",
        "cycleId": bounded(args.cycle_id, 128),
        "invocation": "not-invoked",
        "calls": 0,
        "promptTokens": 0,
        "completionTokens": 0,
        "totalTokens": 0,
        "actualPromptTokens": 0,
        "actualCompletionTokens": 0,
        "actualTotalTokens": 0,
        "plannedModelTier": planned_tier,
        "plannedModelTiers": tiers,
        "accountingConfidence": "explicit_zero",
        "plannedEstimateConfidence": "static_bytes_div_4",
        "invocationReason": "read-only packet construction has no model credentials or write authority",
        "aggregateTargetTokens": MAX_AGGREGATE_PACKET_TOKENS,
        "aggregatePlannedPromptTokens": result["aggregatePlannedPromptTokens"],
        "plannedPromptTokens": result["aggregatePlannedPromptTokens"],
        "modelFree": True,
        "escalationReasons": sorted(
            {
                reason
                for packet in result["packets"]
                for reason in packet.get("escalationReasons", [])
            }
        ),
        "packetBuild": {
            "startedAt": started_at,
            "finishedAt": finished_at,
            "durationMs": duration_ms,
        },
        "status": result["status"],
        "outcome": result["outcome"],
    }
    try:
        _write_packet_artifacts(args.output_dir, result, telemetry, args.evidence)
    except (OSError, CandidatePreflightError, TypeError, ValueError) as error:
        print(f"unable to materialize packet output: {error}", file=sys.stderr)
        return 1
    return 1 if trusted_failure else 0


def write_evidence(args: argparse.Namespace) -> int:
    refs = {"base": args.base, "upstream": args.upstream, "local": args.local}
    if not all(SHA1_RE.fullmatch(value) for value in refs.values()):
        raise CandidatePreflightError("candidate evidence requires exact 40-hex refs")
    if not SHA1_RE.fullmatch(args.workflow_sha):
        raise CandidatePreflightError(
            "candidate evidence requires an exact workflow SHA"
        )
    conflicts = read_conflict_paths(args.conflicts)
    if args.conflict_total < len(conflicts):
        raise CandidatePreflightError(
            "candidate conflict total is smaller than evidence"
        )
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
        "cargoCheck": {
            "status": "not-run",
            "command": "cargo check --workspace --tests --locked",
        },
        "affectedContracts": {"status": "not-run"},
        "rootFailures": {"status": "not-run"},
        "workflow": {"sha": args.workflow_sha, "runId": bounded(args.run_id, 128)},
    }
    args.output_dir.mkdir(parents=True, exist_ok=True)
    json_path = args.output_dir / "candidate-evidence.json"
    text_path = args.output_dir / "candidate-evidence.txt"
    json_path.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
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
        choices=(
            "clean",
            "conflict",
            "infrastructure",
            "preflight-blocked",
            "validation-pending",
        ),
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
    evidence.add_argument(
        "--worktree-removed", choices=("true", "false"), required=True
    )
    evidence.add_argument(
        "--primary-checkout-clean", choices=("true", "false"), required=True
    )
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
    packets = commands.add_parser("build-packets")
    packets.add_argument("--evidence", type=Path, required=True)
    packets.add_argument("--guard", type=Path, required=True)
    packets.add_argument("--gates", type=Path, required=True)
    packets.add_argument("--conflicts", type=Path)
    packets.add_argument("--root-failures", type=Path)
    packets.add_argument("--output-dir", type=Path, required=True)
    packets.add_argument("--cycle-id", required=True)
    packets.add_argument("--started-at", required=True)
    packets.add_argument("--duration-ms", required=True)
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
    if args.command == "build-packets":
        return build_model_packets(args)
    try:
        return write_evidence(args)
    except CandidatePreflightError as error:
        print(bounded(error, MAX_ERROR_LENGTH), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
