#!/usr/bin/env python3

"""Verify machine-readable evidence for the convergence contract matrix."""

import argparse
import json
import re
import sys
from collections import Counter
from datetime import date
from datetime import datetime
from pathlib import Path, PurePosixPath


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = REPO_ROOT / "upstream" / "convergence-gates.json"
DEFAULT_CONTRACTS = REPO_ROOT / "upstream" / "convergence-contracts.md"
SUPPORTED_SCHEMA_VERSION = 1
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_EVIDENCE_FILE_BYTES = 4 * 1024 * 1024
CONTRACT_ID = re.compile(r"[A-Z][A-Z0-9]*-[0-9]+")
CONTRACT_ROW = re.compile(r"^\|\s*`(?P<id>[A-Z][A-Z0-9]*-[0-9]+)`\s*\|")
EVIDENCE_KINDS = {"file", "symbol", "narrative", "semantic_reachability"}
CI_TIERS = {"blocking", "nightly", "release", "narrative"}
SEMANTIC_EDGE_KINDS = {"rust_module", "token"}


class GateManifestError(ValueError):
    """The convergence gate manifest is unsafe or unsupported."""


def require_exact_keys(
    value: dict[str, object], expected: set[str], location: str
) -> None:
    actual = set(value)
    if actual == expected:
        return
    missing = sorted(expected - actual)
    unknown = sorted(actual - expected)
    details = []
    if missing:
        details.append(f"missing {missing}")
    if unknown:
        details.append(f"unknown {unknown}")
    raise GateManifestError(f"{location}: {', '.join(details)}")


def require_string(value: object, location: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise GateManifestError(f"{location}: expected a non-empty string")
    return value


def validate_repo_path(repo_root: Path, value: object, location: str) -> str:
    raw = require_string(value, location)
    if "\\" in raw:
        raise GateManifestError(f"{location}: use a POSIX repository-relative path")
    path = PurePosixPath(raw)
    if path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise GateManifestError(f"{location}: path must stay inside the repository")
    root = repo_root.resolve()
    resolved = (root / Path(*path.parts)).resolve()
    try:
        resolved.relative_to(root)
    except ValueError as error:
        raise GateManifestError(f"{location}: path escapes the repository") from error
    return path.as_posix()


def load_manifest(path: Path, repo_root: Path) -> dict[str, object]:
    resolved = path.resolve()
    try:
        resolved.relative_to(repo_root.resolve())
    except ValueError as error:
        raise GateManifestError(f"manifest path escapes the repository: {resolved}") from error
    try:
        if path.is_symlink():
            raise GateManifestError(f"manifest must not be a symlink: {path}")
        if resolved.stat().st_size > MAX_MANIFEST_BYTES:
            raise GateManifestError(
                f"manifest exceeds {MAX_MANIFEST_BYTES} bytes: {resolved}"
            )
        document = json.loads(resolved.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise GateManifestError(f"missing convergence gate manifest: {path}") from error
    except json.JSONDecodeError as error:
        raise GateManifestError(f"invalid JSON in {path}: {error}") from error
    except (OSError, UnicodeError) as error:
        raise GateManifestError(f"cannot read {path}: {error}") from error
    if not isinstance(document, dict):
        raise GateManifestError(f"{path}: expected a JSON object")
    require_exact_keys(document, {"schemaVersion", "contracts"}, str(path))
    if document["schemaVersion"] != SUPPORTED_SCHEMA_VERSION:
        raise GateManifestError(
            f"{path}: unsupported schemaVersion {document['schemaVersion']!r}; "
            f"expected {SUPPORTED_SCHEMA_VERSION}"
        )
    contracts = document["contracts"]
    if not isinstance(contracts, list) or not contracts:
        raise GateManifestError(f"{path}.contracts: expected a non-empty array")
    return document


def contract_ids_in_markdown(text: str) -> set[str]:
    return {
        match.group("id")
        for line in text.splitlines()
        if (match := CONTRACT_ROW.match(line)) is not None
    }


def verify_evidence(
    repo_root: Path,
    contract_id: str,
    evidence: object,
    index: int,
    errors: list[str],
) -> str | None:
    location = f"{contract_id}.evidence[{index}]"
    if not isinstance(evidence, dict):
        errors.append(f"{location}: expected an object")
        return None
    kind = evidence.get("kind")
    if not isinstance(kind, str) or kind not in EVIDENCE_KINDS:
        errors.append(f"{location}.kind: expected one of {sorted(EVIDENCE_KINDS)}")
        return None
    try:
        if kind == "semantic_reachability":
            return verify_semantic_reachability(repo_root, contract_id, evidence, location, errors)
        expected = (
            {"kind", "description", "issue", "ciTier"}
            if kind == "narrative"
            else {"kind", "path", "ciTier"}
            if kind == "file"
            else {"kind", "path", "token", "ciTier"}
        )
        require_exact_keys(evidence, expected, location)
        tier = require_string(evidence["ciTier"], f"{location}.ciTier")
        if tier not in CI_TIERS:
            raise GateManifestError(
                f"{location}.ciTier: expected one of {sorted(CI_TIERS)}"
            )
        if kind == "narrative":
            require_string(evidence["description"], f"{location}.description")
            issue = evidence["issue"]
            if not isinstance(issue, int) or isinstance(issue, bool) or issue <= 0:
                raise GateManifestError(f"{location}.issue: expected a positive integer")
            if tier not in {"narrative", "release"}:
                raise GateManifestError(
                    f"{location}.ciTier: narrative evidence must be narrative or release"
                )
            return tier
        if tier == "narrative":
            raise GateManifestError(
                f"{location}.ciTier: file-backed evidence cannot be narrative"
            )
        relative = validate_repo_path(repo_root, evidence["path"], f"{location}.path")
        path = repo_root / relative
        if not path.is_file():
            raise GateManifestError(f"{location}.path: file is missing: {relative}")
        if path.is_symlink():
            raise GateManifestError(f"{location}.path: file must not be a symlink: {relative}")
        relative_path = PurePosixPath(relative)
        parts = relative_path.parts
        if (
            "tests" in parts
            and parts.index("tests") + 1 < len(parts)
            and parts[parts.index("tests") + 1] == "suite"
            and relative_path.suffix == ".rs"
            and relative_path.name != "mod.rs"
        ):
            registry = path.parent / "mod.rs"
            module_token = f"mod {relative_path.stem};"
            if not registry.is_file() or module_token not in registry.read_text(
                encoding="utf-8"
            ):
                raise GateManifestError(
                    f"{location}.path: suite proof is not registered with "
                    f"{module_token!r} in {registry.relative_to(repo_root)}"
                )
        if kind == "symbol":
            token = require_string(evidence["token"], f"{location}.token")
            if path.stat().st_size > MAX_EVIDENCE_FILE_BYTES:
                raise GateManifestError(
                    f"{location}.path: file exceeds {MAX_EVIDENCE_FILE_BYTES} bytes"
                )
            try:
                contents = path.read_text(encoding="utf-8")
            except (OSError, UnicodeError) as error:
                raise GateManifestError(f"{location}.path: cannot read {relative}: {error}") from error
            if token not in contents:
                raise GateManifestError(
                    f"{location}.token: {token!r} is absent from {relative}"
                )
        return tier
    except (GateManifestError, OSError) as error:
        errors.append(str(error))
        return None


def rust_module_children(repo_root: Path, module_path: str) -> list[str]:
    path = repo_root / module_path
    if path.stat().st_size > MAX_EVIDENCE_FILE_BYTES:
        raise GateManifestError(
            f"rust module source exceeds {MAX_EVIDENCE_FILE_BYTES} bytes: {module_path}"
        )
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    children: list[str] = []
    for index, line in enumerate(lines):
        match = re.match(
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;",
            line,
        )
        if match is None:
            continue
        module_name = match.group(1)
        path_attr = None
        scan = index - 1
        while scan >= 0 and lines[scan].strip().startswith("#"):
            attr_match = re.match(r'^\s*#\[path\s*=\s*"([^"]+)"\]\s*$', lines[scan])
            if attr_match is not None:
                path_attr = attr_match.group(1)
                break
            scan -= 1
        parent = PurePosixPath(module_path).parent
        stem = PurePosixPath(module_path).stem
        if stem not in {"lib", "main", "mod"}:
            parent /= stem
        if path_attr is not None:
            candidates = [parent / path_attr]
        else:
            candidates = [parent / f"{module_name}.rs", parent / module_name / "mod.rs"]
        for candidate in candidates:
            candidate_path = candidate.as_posix()
            if (repo_root / candidate_path).is_file():
                children.append(candidate_path)
                break
    return children


def reachable_rust_modules(repo_root: Path, root: str) -> set[str]:
    if not (repo_root / root).is_file():
        raise GateManifestError(f"semantic root is missing: {root}")
    reachable = {root}
    pending = [root]
    while pending:
        current = pending.pop()
        for child in rust_module_children(repo_root, current):
            if child not in reachable:
                reachable.add(child)
                pending.append(child)
    return reachable


def semantic_waivers(
    evidence: dict[str, object], location: str
) -> dict[str, dict[str, object]]:
    waivers = evidence["waivers"]
    if not isinstance(waivers, list):
        raise GateManifestError(f"{location}.waivers: expected an array")
    indexed: dict[str, dict[str, object]] = {}
    for index, waiver in enumerate(waivers):
        waiver_location = f"{location}.waivers[{index}]"
        if not isinstance(waiver, dict):
            raise GateManifestError(f"{waiver_location}: expected an object")
        require_exact_keys(
            waiver,
            {"edge", "rationale", "issue", "owner", "expires"},
            waiver_location,
        )
        edge = require_string(waiver["edge"], f"{waiver_location}.edge")
        require_string(waiver["rationale"], f"{waiver_location}.rationale")
        require_string(waiver["owner"], f"{waiver_location}.owner")
        expires = require_string(waiver["expires"], f"{waiver_location}.expires")
        issue = waiver["issue"]
        if not isinstance(issue, int) or isinstance(issue, bool) or issue <= 0:
            raise GateManifestError(f"{waiver_location}.issue: expected a positive integer")
        try:
            expiry = datetime.strptime(expires, "%Y-%m-%d").date()
        except ValueError as error:
            raise GateManifestError(
                f"{waiver_location}.expires: expected YYYY-MM-DD"
            ) from error
        if expiry < date.today():
            raise GateManifestError(f"{waiver_location}.expires: waiver expired on {expires}")
        if edge in indexed:
            raise GateManifestError(f"{waiver_location}.edge: duplicate waiver for {edge}")
        indexed[edge] = waiver
    return indexed


def baseline_waiver_limit(
    repo_root: Path, contract_id: str, baseline_path: str, location: str
) -> int:
    path = repo_root / baseline_path
    if path.is_symlink() or not path.is_file():
        raise GateManifestError(f"{location}: baseline file is missing: {baseline_path}")
    if path.stat().st_size > MAX_MANIFEST_BYTES:
        raise GateManifestError(f"{location}: baseline file exceeds {MAX_MANIFEST_BYTES} bytes")
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
        limit = document["contracts"][contract_id]["maxWaivers"]
    except (KeyError, TypeError, json.JSONDecodeError) as error:
        raise GateManifestError(
            f"{location}: baseline must define contracts.{contract_id}.maxWaivers"
        ) from error
    if document.get("schemaVersion") != 1 or not isinstance(limit, int) or isinstance(limit, bool) or limit < 0:
        raise GateManifestError(f"{location}: invalid semantic reachability baseline")
    return limit


def verify_semantic_edge(
    repo_root: Path,
    edge: dict[str, object],
    location: str,
    rust_cache: dict[str, set[str]],
) -> str | None:
    require_exact_keys(edge, {"id", "kind", "root", "path", "token"}, location)
    edge_id = require_string(edge["id"], f"{location}.id")
    edge_kind = require_string(edge["kind"], f"{location}.kind")
    if edge_kind not in SEMANTIC_EDGE_KINDS:
        raise GateManifestError(
            f"{location}.kind: expected one of {sorted(SEMANTIC_EDGE_KINDS)}"
        )
    root = validate_repo_path(repo_root, edge["root"], f"{location}.root")
    relative = validate_repo_path(repo_root, edge["path"], f"{location}.path")
    token = edge["token"]
    if not isinstance(token, str):
        raise GateManifestError(f"{location}.token: expected a string")
    path = repo_root / relative
    if not path.is_file():
        return f"{edge_id}: semantic edge path is missing: {relative}"
    if path.is_symlink():
        return f"{edge_id}: semantic edge path must not be a symlink: {relative}"
    reachable = rust_cache.setdefault(root, reachable_rust_modules(repo_root, root))
    if relative not in reachable:
        return f"{edge_id}: {relative} is not reachable from Rust module root {root}"
    if edge_kind == "rust_module":
        return None
    if not token:
        raise GateManifestError(f"{location}.token: expected a non-empty string")
    if path.stat().st_size > MAX_EVIDENCE_FILE_BYTES:
        raise GateManifestError(
            f"{location}.path: file exceeds {MAX_EVIDENCE_FILE_BYTES} bytes"
        )
    if token not in path.read_text(encoding="utf-8"):
        return f"{edge_id}: {token!r} is absent from {relative}"
    return None


def verify_semantic_reachability(
    repo_root: Path,
    contract_id: str,
    evidence: dict[str, object],
    location: str,
    errors: list[str],
) -> str | None:
    require_exact_keys(
        evidence,
        {"kind", "description", "baseline", "edges", "waivers", "ciTier"},
        location,
    )
    tier = require_string(evidence["ciTier"], f"{location}.ciTier")
    if tier not in CI_TIERS - {"narrative"}:
        raise GateManifestError(f"{location}.ciTier: expected blocking, nightly, or release")
    require_string(evidence["description"], f"{location}.description")
    baseline = evidence["baseline"]
    if not isinstance(baseline, dict):
        raise GateManifestError(f"{location}.baseline: expected an object")
    require_exact_keys(baseline, {"derivedFrom", "issue", "maxWaivers"}, f"{location}.baseline")
    baseline_path = validate_repo_path(
        repo_root, baseline["derivedFrom"], f"{location}.baseline.derivedFrom"
    )
    issue = baseline["issue"]
    max_waivers = baseline["maxWaivers"]
    if not isinstance(issue, int) or isinstance(issue, bool) or issue <= 0:
        raise GateManifestError(f"{location}.baseline.issue: expected a positive integer")
    if not isinstance(max_waivers, int) or isinstance(max_waivers, bool) or max_waivers < 0:
        raise GateManifestError(f"{location}.baseline.maxWaivers: expected a non-negative integer")
    baseline_limit = baseline_waiver_limit(
        repo_root, contract_id, baseline_path, f"{location}.baseline.derivedFrom"
    )
    if max_waivers > baseline_limit:
        raise GateManifestError(
            f"{location}.baseline.maxWaivers: {max_waivers} exceeds derived baseline {baseline_limit}"
        )
    waivers = semantic_waivers(evidence, location)
    if len(waivers) > max_waivers:
        raise GateManifestError(
            f"{location}.waivers: {len(waivers)} exceeds ratchet maximum {max_waivers}"
        )
    edges = evidence["edges"]
    if not isinstance(edges, list) or not edges:
        raise GateManifestError(f"{location}.edges: expected a non-empty array")
    used_waivers: set[str] = set()
    rust_cache: dict[str, set[str]] = {}
    seen_edges: set[str] = set()
    invalid_edges: set[str] = set()
    for index, edge in enumerate(edges):
        edge_id = None
        edge_location = f"{location}.edges[{index}]"
        if not isinstance(edge, dict):
            errors.append(f"{edge_location}: expected an object")
            continue
        try:
            edge_id = require_string(edge.get("id"), f"{edge_location}.id")
            if edge_id in seen_edges:
                raise GateManifestError(f"{edge_location}.id: duplicate edge {edge_id}")
            seen_edges.add(edge_id)
            failure = verify_semantic_edge(repo_root, edge, edge_location, rust_cache)
        except GateManifestError as error:
            errors.append(str(error))
            if edge_id is not None:
                invalid_edges.add(edge_id)
            continue
        if failure is None:
            continue
        if edge_id in waivers:
            used_waivers.add(edge_id)
        else:
            errors.append(f"{location}: {contract_id} semantic reachability failed: {failure}")
    for edge_id in sorted(set(waivers) - used_waivers - invalid_edges):
        errors.append(f"{location}.waivers: stale waiver for reachable edge {edge_id}")
    return tier


def verify(
    repo_root: Path,
    manifest_path: Path,
    contracts_path: Path,
) -> dict[str, object]:
    errors: list[str] = []
    try:
        document = load_manifest(manifest_path, repo_root)
    except GateManifestError as error:
        return {
            "schemaVersion": 1,
            "contracts": 0,
            "evidence": 0,
            "tierCounts": {},
            "errors": [str(error)],
            "passed": False,
        }
    try:
        resolved_contracts = contracts_path.resolve()
        resolved_contracts.relative_to(repo_root.resolve())
        if contracts_path.is_symlink():
            raise GateManifestError(
                f"convergence contracts must not be a symlink: {contracts_path}"
            )
        if resolved_contracts.stat().st_size > MAX_EVIDENCE_FILE_BYTES:
            raise GateManifestError(
                f"convergence contracts exceed {MAX_EVIDENCE_FILE_BYTES} bytes"
            )
        contracts_text = resolved_contracts.read_text(encoding="utf-8")
    except (GateManifestError, OSError, UnicodeError, ValueError) as error:
        errors.append(f"cannot read convergence contracts: {error}")
        contracts_text = ""
    markdown_ids = contract_ids_in_markdown(contracts_text)
    manifest_ids: set[str] = set()
    evidence_count = 0
    tiers: Counter[str] = Counter()
    for index, contract in enumerate(document["contracts"]):
        location = f"contracts[{index}]"
        if not isinstance(contract, dict):
            errors.append(f"{location}: expected an object")
            continue
        try:
            require_exact_keys(contract, {"id", "evidence"}, location)
            contract_id = require_string(contract["id"], f"{location}.id")
            if CONTRACT_ID.fullmatch(contract_id) is None:
                raise GateManifestError(f"{location}.id: invalid contract ID {contract_id!r}")
            if contract_id in manifest_ids:
                raise GateManifestError(f"{location}.id: duplicate contract ID {contract_id}")
            manifest_ids.add(contract_id)
            evidence_items = contract["evidence"]
            if not isinstance(evidence_items, list) or not evidence_items:
                raise GateManifestError(
                    f"{location}.evidence: expected a non-empty array"
                )
            executable_evidence = 0
            for evidence_index, evidence in enumerate(evidence_items):
                evidence_count += 1
                if isinstance(evidence, dict) and evidence.get("kind") in {
                    "file",
                    "symbol",
                    "semantic_reachability",
                }:
                    executable_evidence += 1
                if tier := verify_evidence(
                    repo_root, contract_id, evidence, evidence_index, errors
                ):
                    tiers[tier] += 1
            if executable_evidence == 0:
                errors.append(
                    f"{location}.evidence: contract {contract_id} must retain at least "
                    "one file-backed evidence item"
                )
        except GateManifestError as error:
            errors.append(str(error))
    missing = sorted(markdown_ids - manifest_ids)
    extra = sorted(manifest_ids - markdown_ids)
    if missing:
        errors.append(f"manifest is missing contract IDs: {missing}")
    if extra:
        errors.append(f"manifest has contract IDs absent from Markdown: {extra}")
    if not markdown_ids:
        errors.append("convergence contract table contains no contract IDs")
    return {
        "schemaVersion": 1,
        "contracts": len(manifest_ids),
        "evidence": evidence_count,
        "tierCounts": dict(sorted(tiers.items())),
        "errors": errors,
        "passed": not errors,
    }


def print_report(report: dict[str, object]) -> None:
    for error in report["errors"]:
        print(f"::error::{error}", file=sys.stderr)
    if report["passed"]:
        print(
            "Upstream convergence gates passed for "
            f"{report['contracts']} contracts and {report['evidence']} evidence items."
        )
        print(f"CI tier inventory: {json.dumps(report['tierCounts'], sort_keys=True)}")
    else:
        print(
            f"Upstream convergence gates failed with {len(report['errors'])} errors.",
            file=sys.stderr,
        )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=str(REPO_ROOT))
    parser.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    parser.add_argument("--contracts", default=str(DEFAULT_CONTRACTS))
    parser.add_argument("--json", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    report = verify(
        Path(args.repo_root).resolve(),
        Path(args.manifest).resolve(),
        Path(args.contracts).resolve(),
    )
    if args.json:
        json.dump(report, sys.stdout, indent=2, sort_keys=True)
        print()
    else:
        print_report(report)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
