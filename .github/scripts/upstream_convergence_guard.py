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
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = REPO_ROOT / "upstream" / "convergence-guard.json"
DEFAULT_WAIVERS = REPO_ROOT / "upstream" / "convergence-waivers.json"

SUPPORTED_MANIFEST_SCHEMA = 1
SUPPORTED_WAIVER_SCHEMA = 1

ABSENT = "absent"
REVERTED = "reverted_to_upstream"
VIOLATIONS = (ABSENT, REVERTED)

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
        value = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise WaiverError(f"missing file: {path}") from error
    except json.JSONDecodeError as error:
        raise WaiverError(f"invalid JSON in {path}: {error}") from error
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
    schema = document.get("schemaVersion")
    if schema != SUPPORTED_MANIFEST_SCHEMA:
        raise WaiverError(
            f"{path}: unsupported manifest schemaVersion {schema!r}; "
            f"expected {SUPPORTED_MANIFEST_SCHEMA}"
        )
    guarded = document.get("guardedPaths")
    if not isinstance(guarded, list):
        raise WaiverError(f"{path}: 'guardedPaths' must be a list")
    return guarded


def evaluate(
    entry: dict[str, object], repo_root: Path
) -> tuple[str, str] | None:
    """Return the violation for one guarded path, or `None` when it is intact."""

    path = str(entry["path"])
    candidate = repo_root / path
    if not candidate.is_file():
        return ABSENT, "owned path is missing from the candidate tree"
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
