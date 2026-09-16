#!/usr/bin/env python3

"""Fail a refresh that silently drops or reverts an owned convergence path.

Merge `9d2eea2238` recorded local history while taking the upstream tree, so
every Every Code-owned file that upstream did not have disappeared without a
single conflict marker. Later merges cannot resurrect those paths, because the
anchor is already their merge base.

This guard reads a checked-in ownership manifest plus an explicit waiver ledger
and fails when a guarded path is absent from the candidate or has reverted to
the recorded upstream blob. It only inspects `intentionally_owned` and
`red_manual_review` paths, so ordinary upstream deletions in the green and amber
lanes stay unblocked.
"""

import argparse
import hashlib
import json
import re
import sys
from collections import Counter
from pathlib import Path, PurePosixPath


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = REPO_ROOT / "upstream" / "convergence-guard.json"
DEFAULT_WAIVERS = REPO_ROOT / "upstream" / "convergence-waivers.json"

SUPPORTED_MANIFEST_SCHEMA = 1
SUPPORTED_WAIVER_SCHEMA = 1
EXPECTED_REPOSITORY = "openai/codex"
EXPECTED_OWNERSHIP_BASELINE = {
    "base": "b89ce9a2bcedcfddf3a48f387b7912d602d6d87c",
    "upstream": "4462b9deef211723b781b426f5e5d36a5777115f",
    "local": "8add494682f7c0674672e8dc5b38a4565cd7629b",
}
EXPECTED_GUARDED_LANES = ("intentionally_owned", "red_manual_review")
EXPECTED_MANIFEST_RULE = (
    "An owned path may not be absent from the candidate, and may not match the "
    "recorded upstream blob, without an explicit waiver."
)
EXPECTED_MANIFEST_SOURCES = {
    "ownership_baseline": (
        "Owned path that already differed from upstream at the pre-anchor local "
        "baseline."
    ),
    "current_tree": (
        "Owned path in the candidate tree, so owned work created or restored "
        "after the baseline is guarded without hand-editing."
    ),
}
EXPECTED_ENTRY_SOURCES = tuple(EXPECTED_MANIFEST_SOURCES)
OBJECT_ID = re.compile(r"[0-9a-f]{40}")
MAX_JSON_BYTES = 8 * 1024 * 1024

ABSENT = "absent"
REVERTED = "reverted_to_upstream"
VIOLATIONS = (ABSENT, REVERTED)

# Manifest rows carrying upstream content that is guarded for existence alone.
PRESENCE_ONLY_GUARD = "presence_only"

DISPOSITIONS = (
    # Upstream deleted the path and Codex Lab accepted the deletion.
    "upstream_deletion_adopted",
    # Upstream converged on the Codex Lab behavior, so the local delta is gone
    # on purpose.
    "converged_with_upstream",
    # The path was lost by the anchor merge and restoring it is tracked work.
    "pending_restore",
)


class WaiverError(ValueError):
    """A waiver ledger entry is unusable, so the guard cannot trust it."""


def blob_id(data: bytes) -> str:
    """Compute the Git blob object id for file contents."""

    header = f"blob {len(data)}\0".encode()
    return hashlib.sha1(header + data).hexdigest()


def load_json(path: Path) -> dict[str, object]:
    try:
        if path.stat().st_size > MAX_JSON_BYTES:
            raise WaiverError(f"file exceeds {MAX_JSON_BYTES} bytes: {path}")
        value = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise WaiverError(f"missing file: {path}") from error
    except json.JSONDecodeError as error:
        raise WaiverError(f"invalid JSON in {path}: {error}") from error
    except (OSError, UnicodeError) as error:
        raise WaiverError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise WaiverError(f"expected a JSON object in {path}")
    return value


def waiver_key(path: str, violation: str) -> tuple[str, str]:
    return (path, violation)


def load_waivers(path: Path) -> dict[tuple[str, str], dict[str, object]]:
    """Index the waiver ledger by `(path, violation)`, rejecting vague entries."""

    document = load_json(path)
    schema = document.get("schemaVersion")
    if schema != SUPPORTED_WAIVER_SCHEMA:
        raise WaiverError(
            f"{path}: unsupported waiver schemaVersion {schema!r}; "
            f"expected {SUPPORTED_WAIVER_SCHEMA}"
        )
    entries = document.get("waivers")
    if not isinstance(entries, list):
        raise WaiverError(f"{path}: 'waivers' must be a list")

    waivers: dict[tuple[str, str], dict[str, object]] = {}
    for index, entry in enumerate(entries):
        location = f"{path}: waiver {index}"
        if not isinstance(entry, dict):
            raise WaiverError(f"{location}: expected an object")
        waived_path = entry.get("path")
        violation = entry.get("violation")
        disposition = entry.get("disposition")
        reason = entry.get("reason")
        issue = entry.get("issue")
        if not isinstance(waived_path, str) or not waived_path:
            raise WaiverError(f"{location}: 'path' must be a non-empty string")
        if violation not in VIOLATIONS:
            raise WaiverError(
                f"{location}: 'violation' must be one of {', '.join(VIOLATIONS)}"
            )
        if disposition not in DISPOSITIONS:
            raise WaiverError(
                f"{location}: 'disposition' must be one of {', '.join(DISPOSITIONS)}"
            )
        if not isinstance(reason, str) or not reason.strip():
            raise WaiverError(f"{location}: 'reason' must be a non-empty string")
        if not isinstance(issue, int):
            raise WaiverError(f"{location}: 'issue' must be the deciding issue number")
        key = waiver_key(waived_path, violation)
        if key in waivers:
            raise WaiverError(
                f"{location}: duplicate waiver for {waived_path} ({violation})"
            )
        waivers[key] = entry
    return waivers


def load_manifest(path: Path) -> list[dict[str, object]]:
    document = load_json(path)
    expected_keys = {
        "schemaVersion",
        "repository",
        "ownershipBaseline",
        "policy",
        "summary",
        "guardedPaths",
    }
    if set(document) != expected_keys:
        raise WaiverError(f"{path}: manifest keys must be {sorted(expected_keys)}")
    schema = document.get("schemaVersion")
    if schema != SUPPORTED_MANIFEST_SCHEMA:
        raise WaiverError(
            f"{path}: unsupported manifest schemaVersion {schema!r}; "
            f"expected {SUPPORTED_MANIFEST_SCHEMA}"
        )
    if document.get("repository") != EXPECTED_REPOSITORY:
        raise WaiverError(f"{path}: repository must be {EXPECTED_REPOSITORY}")
    ownership_baseline = document.get("ownershipBaseline")
    if not isinstance(ownership_baseline, dict):
        raise WaiverError(f"{path}: ownershipBaseline must be an object")
    pinned_baseline = {
        key: ownership_baseline.get(key) for key in EXPECTED_OWNERSHIP_BASELINE
    }
    if pinned_baseline != EXPECTED_OWNERSHIP_BASELINE:
        raise WaiverError(f"{path}: immutable ownership baseline changed")
    baseline_keys = set(ownership_baseline)
    allowed_baseline_keys = {*EXPECTED_OWNERSHIP_BASELINE, "current"}
    if not baseline_keys <= allowed_baseline_keys:
        raise WaiverError(
            f"{path}: ownershipBaseline keys must be a subset of "
            f"{sorted(allowed_baseline_keys)}"
        )
    current = ownership_baseline.get("current")
    if current is not None and (
        not isinstance(current, str) or OBJECT_ID.fullmatch(current) is None
    ):
        raise WaiverError(f"{path}: ownershipBaseline.current must be a commit ID")
    policy = document.get("policy")
    if not isinstance(policy, dict):
        raise WaiverError(f"{path}: policy must be an object")
    if (
        policy.get("guardedLanes") != list(EXPECTED_GUARDED_LANES)
        or policy.get("rule") != EXPECTED_MANIFEST_RULE
    ):
        raise WaiverError(f"{path}: guard policy changed unexpectedly")
    allowed_policy_keys = {"guardedLanes", "rule", "sources"}
    if not set(policy) <= allowed_policy_keys:
        raise WaiverError(
            f"{path}: policy keys must be a subset of {sorted(allowed_policy_keys)}"
        )
    sources = policy.get("sources")
    if sources is not None and sources != EXPECTED_MANIFEST_SOURCES:
        raise WaiverError(f"{path}: guard sources changed unexpectedly")
    guarded = document.get("guardedPaths")
    if not isinstance(guarded, list):
        raise WaiverError(f"{path}: 'guardedPaths' must be a list")
    seen = set()
    for index, entry in enumerate(guarded):
        location = f"{path}: guarded path {index}"
        if not isinstance(entry, dict):
            raise WaiverError(f"{location}: expected an object")
        required_entry_keys = {
            "path",
            "lane",
            "contracts",
            "reason",
            "baselineBlob",
            "upstreamBlob",
        }
        optional_entry_keys = {"source", "guard"}
        if not required_entry_keys <= set(entry) or not set(entry) <= (
            required_entry_keys | optional_entry_keys
        ):
            raise WaiverError(
                f"{location}: entry keys must include {sorted(required_entry_keys)} "
                f"and may include {sorted(optional_entry_keys)}"
            )
        guarded_path = entry.get("path")
        if not isinstance(guarded_path, str) or not guarded_path:
            raise WaiverError(f"{location}: 'path' must be a non-empty string")
        if "\\" in guarded_path:
            raise WaiverError(f"{location}: use a POSIX repository-relative path")
        parsed = PurePosixPath(guarded_path)
        if parsed.is_absolute() or ".." in parsed.parts or "." in parsed.parts:
            raise WaiverError(f"{location}: path must stay inside the repository")
        if guarded_path in seen:
            raise WaiverError(f"{location}: duplicate path {guarded_path}")
        seen.add(guarded_path)
        lane = entry.get("lane")
        if lane not in EXPECTED_GUARDED_LANES:
            raise WaiverError(
                f"{location}: lane must be one of {', '.join(EXPECTED_GUARDED_LANES)}"
            )
        contracts = entry.get("contracts")
        if (
            not isinstance(contracts, list)
            or not contracts
            or not all(isinstance(contract, str) and contract for contract in contracts)
        ):
            raise WaiverError(f"{location}: contracts must be non-empty strings")
        reason = entry.get("reason")
        if not isinstance(reason, str) or not reason.strip():
            raise WaiverError(f"{location}: reason must be a non-empty string")
        baseline_blob = entry.get("baselineBlob")
        if (
            not isinstance(baseline_blob, str)
            or OBJECT_ID.fullmatch(baseline_blob) is None
        ):
            raise WaiverError(f"{location}: baselineBlob must be a Git object ID")
        upstream_blob = entry.get("upstreamBlob")
        if upstream_blob is not None and (
            not isinstance(upstream_blob, str)
            or OBJECT_ID.fullmatch(upstream_blob) is None
        ):
            raise WaiverError(
                f"{location}: upstreamBlob must be null or a Git object ID"
            )
        source = entry.get("source")
        if current is not None and source not in EXPECTED_ENTRY_SOURCES:
            raise WaiverError(
                f"{location}: source must be one of {', '.join(EXPECTED_ENTRY_SOURCES)}"
            )
        if current is None and source is not None:
            raise WaiverError(f"{location}: legacy manifests must not declare source")
        guard = entry.get("guard")
        if guard is not None and guard != PRESENCE_ONLY_GUARD:
            raise WaiverError(
                f"{location}: guard must be {PRESENCE_ONLY_GUARD!r} when present"
            )

    summary = document.get("summary")
    expected_summary = {
        "guardedPaths": len(guarded),
        "guardedLaneCounts": dict(
            sorted(Counter(entry["lane"] for entry in guarded).items())
        ),
    }
    if current is not None:
        expected_summary["guardedSourceCounts"] = dict(
            sorted(Counter(entry["source"] for entry in guarded).items())
        )
    if summary != expected_summary:
        raise WaiverError(f"{path}: summary does not match guardedPaths")
    return guarded


def candidate_path(repo_root: Path, guarded_path: str) -> tuple[Path, str | None]:
    root = repo_root.resolve()
    candidate = root
    for part in PurePosixPath(guarded_path).parts:
        candidate /= part
        if candidate.is_symlink():
            return candidate, "owned path contains a symbolic link"
    try:
        candidate.resolve(strict=False).relative_to(root)
    except ValueError:
        return candidate, "owned path resolves outside the repository"
    return candidate, None


def evaluate(entry: dict[str, object], repo_root: Path) -> tuple[str, str] | None:
    """Return the violation for one guarded path, or `None` when it is intact."""

    path = str(entry["path"])
    candidate, unsafe_detail = candidate_path(repo_root, path)
    if unsafe_detail is not None:
        return ABSENT, unsafe_detail
    if not candidate.is_file():
        return ABSENT, "owned path is missing from the candidate tree"
    # A presence-only row records a path whose content is upstream's. It is
    # guarded because deleting it unregisters the owned suites it declares, not
    # because its bytes are locally owned, so comparing them would only produce
    # a violation for the intact state.
    if entry.get("guard") == PRESENCE_ONLY_GUARD:
        return None
    upstream_blob = entry.get("upstreamBlob")
    if not isinstance(upstream_blob, str):
        return None
    if blob_id(candidate.read_bytes()) == upstream_blob:
        return REVERTED, "owned path is byte-identical to the recorded upstream blob"
    return None


def check(
    manifest: list[dict[str, object]],
    waivers: dict[tuple[str, str], dict[str, object]],
    repo_root: Path,
) -> dict[str, object]:
    violations: list[dict[str, object]] = []
    waived: list[dict[str, object]] = []
    used: set[tuple[str, str]] = set()

    for entry in manifest:
        result = evaluate(entry, repo_root)
        if result is None:
            continue
        violation, detail = result
        record = {
            "path": entry["path"],
            "lane": entry["lane"],
            "contracts": entry.get("contracts", []),
            "violation": violation,
            "detail": detail,
        }
        key = waiver_key(str(entry["path"]), violation)
        waiver = waivers.get(key)
        if waiver is None:
            violations.append(record)
            continue
        used.add(key)
        waived.append(
            {
                **record,
                "disposition": waiver["disposition"],
                "issue": waiver["issue"],
                "reason": waiver["reason"],
            }
        )

    stale = [
        {
            "path": path,
            "violation": violation,
            "detail": "waiver no longer matches any violation; delete it",
        }
        for path, violation in sorted(set(waivers) - used)
    ]

    return {
        "guardedPaths": len(manifest),
        "violations": violations,
        "waived": waived,
        "staleWaivers": stale,
        "passed": not violations and not stale,
    }


def print_report(report: dict[str, object]) -> None:
    print(f"Guarded owned paths: {report['guardedPaths']}")
    print(f"Waived violations: {len(report['waived'])}")
    for record in report["violations"]:
        contracts = ", ".join(record["contracts"]) or "none"
        print(
            f"::error file={record['path']}::"
            f"{record['violation']}: {record['detail']} "
            f"(lane {record['lane']}, contracts {contracts})",
            file=sys.stderr,
        )
    for record in report["staleWaivers"]:
        print(
            f"::error file={record['path']}::"
            f"stale waiver for {record['violation']}: {record['detail']}",
            file=sys.stderr,
        )
    if report["passed"]:
        print("Upstream convergence guard passed.")
        return
    print(
        f"Upstream convergence guard failed: {len(report['violations'])} "
        f"unwaived violations, {len(report['staleWaivers'])} stale waivers.",
        file=sys.stderr,
    )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    parser.add_argument("--waivers", default=str(DEFAULT_WAIVERS))
    parser.add_argument("--repo-root", default=str(REPO_ROOT))
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit the machine-readable guard report on stdout",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        manifest = load_manifest(Path(args.manifest))
        waivers = load_waivers(Path(args.waivers))
    except WaiverError as error:
        print(f"::error::{error}", file=sys.stderr)
        return 2
    report = check(manifest, waivers, Path(args.repo_root).resolve())
    if args.json:
        json.dump(report, sys.stdout, indent=2, sort_keys=True)
        print()
    else:
        print_report(report)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
