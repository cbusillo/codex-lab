#!/usr/bin/env python3
"""Validate and render durable semantic classifications for one upstream range."""

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import sys

SCHEMA_VERSION = 1
AUDIT_SCHEMA_VERSION = 2
LEDGER_KIND = "codex_lab_upstream_semantic_ledger"
SUMMARY_KIND = "codex_lab_upstream_semantic_summary"
AUDIT_BUCKETS = "app_server core tui history_storage_protocol other_codex_rs repository_tooling other".split()
AUDIT_MECHANICAL_STATUSES = (
    "exact_commit patch_equivalent missing_patch uncomparable".split()
)
DISPOSITIONS = {
    "pre_checkpoint": ("adopted", "adapted", "not_applicable", "missing", "unclear"),
    "post_checkpoint": "adopt adapt defer not_applicable reject decision_required unclear".split(),
}
BLOCKERS = {
    "pre_checkpoint": {"missing", "unclear"},
    "post_checkpoint": {"decision_required", "unclear"},
}
IMPLEMENTATION_STATUSES = (
    "implemented partially_implemented missing not_required not_evaluated".split()
)
AREAS = (*AUDIT_BUCKETS, "cross_cutting")
EVIDENCE_KINDS = ("github_issue", "github_pull_request", "git_commit")
MECHANICAL_COUNT_KEYS = {
    "exact_commit": "exactCommitCount",
    "patch_equivalent": "patchEquivalentCommitCount",
    "missing_patch": "missingPatchCount",
    "uncomparable": "uncomparableCommitCount",
}
MAX_INPUT_BYTES = 5_000_000
MAX_AUDIT_COMMITS = 50_000
MAX_ROWS = 5_000
MAX_STACK_COMMITS = 25
MAX_EVIDENCE = 8
FULL_SHA = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
STACK_ID = re.compile(r"^[a-z0-9][a-z0-9._-]*$")
GITHUB_REF = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+#[1-9][0-9]*$")
COMMIT_REF = re.compile(
    r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+@(?:[0-9a-f]{40}|[0-9a-f]{64})$"
)
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


class LedgerError(RuntimeError):
    pass


def fail(path: str, message: str) -> None:
    raise LedgerError(f"{path}: {message}")


def obj(
    value: object,
    path: str,
    required: tuple[str, ...],
    optional: tuple[str, ...] = (),
) -> dict[str, object]:
    if not isinstance(value, dict):
        fail(path, "must be an object")
    keys = set(value)
    missing = sorted(set(required) - keys)
    extra = sorted(keys - set(required) - set(optional))
    if missing or extra:
        detail = f"missing {missing}" if missing else f"unknown {extra}"
        fail(path, detail)
    return value


def array(value: object, path: str, maximum: int, minimum: int = 0) -> list[object]:
    if not isinstance(value, list) or not minimum <= len(value) <= maximum:
        fail(path, f"must contain between {minimum} and {maximum} items")
    return value


def text(value: object, path: str, maximum: int, *, one_line: bool = False) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum:
        fail(path, f"must be a non-empty string of at most {maximum} characters")
    if one_line and any(character in value for character in "\r\n"):
        fail(path, "must not contain newlines")
    return value


def count(value: object, path: str) -> int:
    if type(value) is not int or value < 0:
        fail(path, "must be a non-negative integer")
    return value


def sha(value: object, path: str) -> str:
    result = text(value, path, 64)
    if FULL_SHA.fullmatch(result) is None:
        fail(path, "must be a full lowercase commit SHA")
    return result


def load(path: Path, label: str) -> object:
    try:
        if path.stat().st_size > MAX_INPUT_BYTES:
            fail(label, f"input exceeds {MAX_INPUT_BYTES} bytes")
        return json.loads(path.read_text(encoding="utf-8"))
    except LedgerError:
        raise
    except (OSError, UnicodeError):
        fail(label, "could not read input")
    except json.JSONDecodeError as error:
        fail(label, f"invalid JSON at line {error.lineno} column {error.colno}")


def validate_range(
    value: object,
    path: str,
    expected_after: str,
    expected_through: str,
) -> dict[str, object]:
    range_value = obj(
        value,
        path,
        "after commitCount commits patchEquivalence primaryPathBuckets through".split(),
    )
    after = sha(range_value["after"], f"{path}.after")
    through = sha(range_value["through"], f"{path}.through")
    if (after, through) != (expected_after, expected_through):
        fail(path, "boundaries do not match audit metadata")
    commit_total = count(range_value["commitCount"], f"{path}.commitCount")
    values = array(range_value["commits"], f"{path}.commits", MAX_AUDIT_COMMITS)
    if len(values) != commit_total:
        fail(f"{path}.commits", "length must equal commitCount")
    commits = []
    mechanical = Counter()
    buckets = Counter()
    seen = set()
    for index, value in enumerate(values):
        row_path = f"{path}.commits[{index}]"
        item = obj(value, row_path, ("mechanicalStatus", "primaryPathBucket", "sha"))
        commit = sha(item["sha"], f"{row_path}.sha")
        status = text(item["mechanicalStatus"], f"{row_path}.mechanicalStatus", 32)
        bucket = text(item["primaryPathBucket"], f"{row_path}.primaryPathBucket", 64)
        if commit in seen or status not in AUDIT_MECHANICAL_STATUSES:
            fail(row_path, "has a duplicate commit or unsupported mechanical status")
        if bucket not in AUDIT_BUCKETS:
            fail(f"{row_path}.primaryPathBucket", "has an unsupported value")
        seen.add(commit)
        mechanical[status] += 1
        buckets[bucket] += 1
        commits.append(
            {"sha": commit, "mechanicalStatus": status, "primaryPathBucket": bucket}
        )
    patch = obj(
        range_value["patchEquivalence"],
        f"{path}.patchEquivalence",
        (
            "algorithm equivalenceScope exactCommitCount missingPatchCount "
            "patchEquivalentCommitCount uncomparableCommitCount"
        ).split(),
    )
    if (
        text(patch["algorithm"], f"{path}.patchEquivalence.algorithm", 64),
        text(
            patch["equivalenceScope"], f"{path}.patchEquivalence.equivalenceScope", 64
        ),
    ) != (
        "git-reachability-and-cherry-v1",
        "exact_commit_or-git-cherry-patch-only",
    ):
        fail(f"{path}.patchEquivalence", "has an unsupported algorithm or scope")
    for status, key in MECHANICAL_COUNT_KEYS.items():
        if count(patch[key], f"{path}.patchEquivalence.{key}") != mechanical[status]:
            fail(f"{path}.patchEquivalence.{key}", "does not match commit rows")
    reported_buckets = obj(
        range_value["primaryPathBuckets"],
        f"{path}.primaryPathBuckets",
        AUDIT_BUCKETS,
    )
    for bucket in AUDIT_BUCKETS:
        if (
            count(reported_buckets[bucket], f"{path}.primaryPathBuckets.{bucket}")
            != buckets[bucket]
        ):
            fail(f"{path}.primaryPathBuckets.{bucket}", "does not match commit rows")
    if (not commits and after != through) or (
        commits and (commits[-1]["sha"] != through or after in seen)
    ):
        fail(f"{path}.commits", "do not match exclusive after/through boundaries")
    return {
        "after": after,
        "commits": commits,
        "index": {item["sha"]: index for index, item in enumerate(commits)},
        "through": through,
    }


def validate_audit(value: object) -> dict[str, object]:
    root = obj(
        value, "audit", ("implementation", "ranges", "schemaVersion", "upstream")
    )
    if count(root["schemaVersion"], "audit.schemaVersion") != AUDIT_SCHEMA_VERSION:
        fail("audit.schemaVersion", f"must equal {AUDIT_SCHEMA_VERSION}")
    implementation = obj(
        root["implementation"],
        "audit.implementation",
        (
            "baseline",
            "mergeBaseWithClassifiedCheckpoint",
            "mergeBaseWithObservedUpstream",
        ),
    )
    upstream = obj(
        root["upstream"],
        "audit.upstream",
        ("adoptedCheckpoint", "branch", "classifiedCheckpoint", "observedHead"),
    )
    baseline = sha(implementation["baseline"], "audit.implementation.baseline")
    classified_merge_base = sha(
        implementation["mergeBaseWithClassifiedCheckpoint"],
        "audit.implementation.mergeBaseWithClassifiedCheckpoint",
    )
    sha(
        implementation["mergeBaseWithObservedUpstream"],
        "audit.implementation.mergeBaseWithObservedUpstream",
    )
    classified = sha(
        upstream["classifiedCheckpoint"], "audit.upstream.classifiedCheckpoint"
    )
    observed = sha(upstream["observedHead"], "audit.upstream.observedHead")
    if upstream["adoptedCheckpoint"] is not None:
        sha(upstream["adoptedCheckpoint"], "audit.upstream.adoptedCheckpoint")
    branch = text(upstream["branch"], "audit.upstream.branch", 255)
    ranges = obj(root["ranges"], "audit.ranges", ("postCheckpoint", "preCheckpoint"))
    normalized = {
        "pre_checkpoint": validate_range(
            ranges["preCheckpoint"],
            "audit.ranges.preCheckpoint",
            classified_merge_base,
            classified,
        ),
        "post_checkpoint": validate_range(
            ranges["postCheckpoint"],
            "audit.ranges.postCheckpoint",
            classified,
            observed,
        ),
    }
    if set(normalized["pre_checkpoint"]["index"]) & set(
        normalized["post_checkpoint"]["index"]
    ):
        fail("audit.ranges", "pre- and post-checkpoint commits must not overlap")
    return {"baseline": baseline, "branch": branch, "ranges": normalized}


def validate_evidence(value: object, path: str) -> dict[str, str]:
    item = obj(value, path, ("kind", "reference"))
    kind = text(item["kind"], f"{path}.kind", 32)
    reference = text(item["reference"], f"{path}.reference", 300)
    if kind not in EVIDENCE_KINDS:
        fail(f"{path}.kind", "has an unsupported value")
    if kind in {"github_issue", "github_pull_request"}:
        valid = GITHUB_REF.fullmatch(reference) is not None
    else:
        valid = COMMIT_REF.fullmatch(reference) is not None
    if not valid:
        fail(f"{path}.reference", f"is invalid for evidence kind {kind}")
    return {"kind": kind, "reference": reference}


def validate_ledger(
    value: object, audit: dict[str, object]
) -> tuple[dict[str, object], list[dict[str, object]]]:
    root = obj(value, "ledger", ("kind", "range", "rows", "schemaVersion", "upstream"))
    if root["kind"] != LEDGER_KIND:
        fail("ledger.kind", f"must equal {LEDGER_KIND}")
    if count(root["schemaVersion"], "ledger.schemaVersion") != SCHEMA_VERSION:
        fail("ledger.schemaVersion", f"must equal {SCHEMA_VERSION}")
    upstream = obj(root["upstream"], "ledger.upstream", ("branch", "repository"))
    branch = text(upstream["branch"], "ledger.upstream.branch", 255)
    repository = text(upstream["repository"], "ledger.upstream.repository", 120)
    if branch != audit["branch"] or REPOSITORY.fullmatch(repository) is None:
        fail(
            "ledger.upstream",
            "does not match the audit branch or owner/repository form",
        )
    range_value = obj(
        root["range"],
        "ledger.range",
        ("after", "implementationBaseline", "through", "window"),
    )
    window = text(range_value["window"], "ledger.range.window", 32)
    if window not in DISPOSITIONS:
        fail("ledger.range.window", "has an unsupported value")
    selected_range = audit["ranges"][window]
    for key, expected in (
        ("after", selected_range["after"]),
        ("implementationBaseline", audit["baseline"]),
        ("through", selected_range["through"]),
    ):
        if sha(range_value[key], f"ledger.range.{key}") != expected:
            fail(f"ledger.range.{key}", "does not match audit")
    rows = array(root["rows"], "ledger.rows", MAX_ROWS)
    covered = set()
    blocking = set()
    stack_ids = set()
    dispositions = Counter()
    normalized = []
    audit_index = selected_range["index"]
    for index, value in enumerate(rows):
        path = f"ledger.rows[{index}]"
        row = obj(
            value,
            path,
            "area commits disposition evidence implementationStatus summary".split(),
            ("stackId",),
        )
        commits = [
            sha(commit, f"{path}.commits[{commit_index}]")
            for commit_index, commit in enumerate(
                array(row["commits"], f"{path}.commits", MAX_STACK_COMMITS, 1)
            )
        ]
        positions = [audit_index.get(commit) for commit in commits]
        if (
            len(set(commits)) != len(commits)
            or any(position is None for position in positions)
            or positions != sorted(positions)
            or covered.intersection(commits)
        ):
            fail(
                f"{path}.commits",
                "must be unique, ordered, in-range, and non-overlapping",
            )
        covered.update(commits)
        stack_id = row.get("stackId")
        if len(commits) == 1 and stack_id is not None:
            fail(f"{path}.stackId", "is only valid for multi-commit stacks")
        if len(commits) > 1:
            stack_id = text(stack_id, f"{path}.stackId", 80)
            if STACK_ID.fullmatch(stack_id) is None or stack_id in stack_ids:
                fail(f"{path}.stackId", "must be a unique lowercase slug")
            stack_ids.add(stack_id)
        disposition = text(row["disposition"], f"{path}.disposition", 32)
        implementation = text(
            row["implementationStatus"], f"{path}.implementationStatus", 32
        )
        area = text(row["area"], f"{path}.area", 64)
        summary = text(row["summary"], f"{path}.summary", 500, one_line=True)
        if disposition not in DISPOSITIONS[window]:
            fail(f"{path}.disposition", "has an unsupported value for its window")
        if implementation not in IMPLEMENTATION_STATUSES or area not in AREAS:
            fail(path, "has an unsupported implementation status or area")
        if (
            disposition in {"not_applicable", "reject"}
            and implementation != "not_required"
        ):
            fail(f"{path}.implementationStatus", "must be not_required")
        evidence = [
            validate_evidence(item, f"{path}.evidence[{evidence_index}]")
            for evidence_index, item in enumerate(
                array(row["evidence"], f"{path}.evidence", MAX_EVIDENCE, 1)
            )
        ]
        dispositions[disposition] += len(commits)
        if disposition in BLOCKERS[window]:
            blocking.update(commits)
        normalized.append(
            {
                "area": area,
                "commits": commits,
                "disposition": disposition,
                "evidence": evidence,
                "implementationStatus": implementation,
                "mechanical": [
                    selected_range["commits"][audit_index[commit]] for commit in commits
                ],
                "stackId": stack_id,
                "summary": summary,
            }
        )
    normalized.sort(key=lambda row: audit_index[row["commits"][0]])
    audit_commits = [item["sha"] for item in selected_range["commits"]]
    missing = [commit for commit in audit_commits if commit not in covered]
    blocked = [commit for commit in audit_commits if commit in blocking]
    summary = {
        "kind": SUMMARY_KIND,
        "range": {
            "after": selected_range["after"],
            "auditCommitCount": len(audit_commits),
            "blockingCommitCount": len(blocked),
            "blockingCommits": blocked,
            "classifiedCommitCount": len(covered),
            "complete": not missing and not blocked,
            "dispositions": {name: dispositions[name] for name in DISPOSITIONS[window]},
            "implementationBaseline": audit["baseline"],
            "missingCommitCount": len(missing),
            "missingCommits": missing,
            "through": selected_range["through"],
            "window": window,
        },
        "schemaVersion": SCHEMA_VERSION,
        "upstream": {"branch": branch, "repository": repository},
    }
    return summary, normalized


def render(summary: dict[str, object], rows: list[dict[str, object]]) -> str:
    range_summary = summary["range"]
    window = range_summary["window"]
    lines = [
        "# Upstream Semantic Ledger",
        "",
        f"- Repository: `{summary['upstream']['repository']}`",
        f"- Branch: `{summary['upstream']['branch']}`",
        f"- Window: `{window}`",
        f"- Range: `{range_summary['after']}..{range_summary['through']}`",
        f"- Complete: {'yes' if range_summary['complete'] else 'no'}",
        "",
        "## Coverage",
        "",
        "| Measure | Commits |",
        "|---|---:|",
        f"| Audit | {range_summary['auditCommitCount']} |",
        f"| Classified | {range_summary['classifiedCommitCount']} |",
        f"| Missing | {range_summary['missingCommitCount']} |",
        f"| Blocking | {range_summary['blockingCommitCount']} |",
        "",
        "## Classifications",
        "",
        "| Subject | Area | Mechanical / Path | Disposition | Implementation | Summary | Evidence |",
        "|---|---|---|---|---|---|---|",
    ]
    for row in rows:
        if row["stackId"]:
            commits = ", ".join(f"`{commit[:12]}`" for commit in row["commits"])
            subject = f"`{row['stackId']}` ({commits})"
        else:
            subject = f"`{row['commits'][0]}`"
        evidence = ", ".join(
            f"{item['kind']}:{item['reference']}".replace("|", "\\|")
            for item in row["evidence"]
        )
        mechanical = ", ".join(
            f"`{item['sha'][:12]}:{item['mechanicalStatus']}/{item['primaryPathBucket']}`"
            for item in row["mechanical"]
        )
        lines.append(
            "| "
            + " | ".join(
                (
                    subject,
                    f"`{row['area']}`",
                    mechanical,
                    f"`{row['disposition']}`",
                    f"`{row['implementationStatus']}`",
                    row["summary"].replace("|", "\\|"),
                    evidence,
                )
            )
            + " |"
        )
    for heading, key in (
        ("Missing Commits", "missingCommits"),
        ("Blocking Commits", "blockingCommits"),
    ):
        if range_summary[key]:
            lines.extend(["", f"## {heading}", ""])
            lines.extend(f"- `{commit}`" for commit in range_summary[key])
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("validate", "render"):
        subparser = subparsers.add_parser(command)
        subparser.add_argument("--audit", required=True, type=Path)
        subparser.add_argument("--ledger", required=True, type=Path)
        subparser.add_argument("--require-complete", action="store_true")
    return parser.parse_args()


def main() -> int:
    config = parse_args()
    try:
        audit = validate_audit(load(config.audit, "audit"))
        summary, rows = validate_ledger(load(config.ledger, "ledger"), audit)
    except LedgerError as error:
        print(f"upstream semantic ledger failed: {error}", file=sys.stderr)
        return 1
    if config.command == "validate":
        print(json.dumps(summary, ensure_ascii=True, indent=2, sort_keys=True))
    else:
        print(render(summary, rows), end="")
    if config.require_complete and not summary["range"]["complete"]:
        print("upstream semantic ledger is incomplete", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
