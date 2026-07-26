#!/usr/bin/env python3

"""Verify the repository-owned upstream convergence control plane."""

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

import upstream_convergence_inventory as inventory


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_POLICY = REPO_ROOT / "upstream" / "convergence-policy.json"
SUPPORTED_POLICY_SCHEMA = 1

GOVERNANCE_PATHS = (
    "AGENTS.md",
    "upstream/README.md",
    "upstream/convergence-contracts.md",
    "upstream/convergence-policy.json",
    "upstream/convergence-guard.json",
    "upstream/convergence-waivers.json",
    ".github/scripts/upstream_convergence.py",
    ".github/scripts/upstream_convergence_guard.py",
    ".github/scripts/upstream_convergence_inventory.py",
    ".github/scripts/verify_upstream_convergence_governance.py",
    ".github/scripts/test_upstream_convergence.py",
    ".github/scripts/test_upstream_convergence_guard.py",
    ".github/scripts/test_upstream_convergence_inventory.py",
    ".github/scripts/test_upstream_convergence_governance.py",
    ".github/scripts/test_convergence_guard_workflows.py",
    ".github/workflows/blocking-ci.yml",
    ".github/workflows/repo-checks.yml",
)


class PolicyError(ValueError):
    """The convergence discovery manifest is unsafe or unsupported."""


@dataclass(frozen=True)
class ConvergencePolicy:
    repository: str
    remote: str
    branch: str
    allowed_fetch_urls: tuple[str, ...]
    contracts_path: str
    evidence_root: str
    plan_issue: str


def require_exact_keys(
    value: dict[str, object], expected: set[str], location: str
) -> None:
    actual = set(value)
    if actual == expected:
        return
    missing = sorted(expected - actual)
    unknown = sorted(actual - expected)
    detail = []
    if missing:
        detail.append(f"missing {missing}")
    if unknown:
        detail.append(f"unknown {unknown}")
    raise PolicyError(f"{location}: {', '.join(detail)}")


def require_string(value: object, location: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise PolicyError(f"{location}: expected a non-empty string")
    return value


def validate_repo_path(repo_root: Path, value: object, location: str) -> str:
    raw = require_string(value, location)
    if "\\" in raw:
        raise PolicyError(f"{location}: use a POSIX repository-relative path")
    path = PurePosixPath(raw)
    if path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise PolicyError(f"{location}: path must stay inside the repository")
    root = repo_root.resolve()
    resolved = (root / Path(*path.parts)).resolve()
    try:
        resolved.relative_to(root)
    except ValueError as error:
        raise PolicyError(f"{location}: path escapes the repository") from error
    return path.as_posix()


def load_policy(path: Path, repo_root: Path) -> ConvergencePolicy:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise PolicyError(f"missing convergence policy: {path}") from error
    except json.JSONDecodeError as error:
        raise PolicyError(f"invalid JSON in {path}: {error}") from error
    if not isinstance(document, dict):
        raise PolicyError(f"{path}: expected a JSON object")
    require_exact_keys(
        document,
        {
            "schemaVersion",
            "upstream",
            "contractsPath",
            "evidenceRoot",
            "planIssue",
        },
        str(path),
    )
    if document["schemaVersion"] != SUPPORTED_POLICY_SCHEMA:
        raise PolicyError(
            f"{path}: unsupported schemaVersion {document['schemaVersion']!r}; "
            f"expected {SUPPORTED_POLICY_SCHEMA}"
        )

    upstream = document["upstream"]
    if not isinstance(upstream, dict):
        raise PolicyError(f"{path}: upstream must be an object")
    require_exact_keys(
        upstream,
        {"repository", "remote", "branch", "allowedFetchUrls"},
        f"{path}: upstream",
    )
    repository = require_string(upstream["repository"], f"{path}: upstream.repository")
    if re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) is None:
        raise PolicyError(f"{path}: upstream.repository must be OWNER/REPO")
    remote = require_string(upstream["remote"], f"{path}: upstream.remote")
    branch = require_string(upstream["branch"], f"{path}: upstream.branch")
    raw_urls = upstream["allowedFetchUrls"]
    if not isinstance(raw_urls, list) or not raw_urls:
        raise PolicyError(f"{path}: upstream.allowedFetchUrls must be a non-empty list")
    urls = tuple(
        require_string(value, f"{path}: upstream.allowedFetchUrls")
        for value in raw_urls
    )

    return ConvergencePolicy(
        repository=repository,
        remote=remote,
        branch=branch,
        allowed_fetch_urls=urls,
        contracts_path=validate_repo_path(
            repo_root, document["contractsPath"], f"{path}: contractsPath"
        ),
        evidence_root=validate_repo_path(
            repo_root, document["evidenceRoot"], f"{path}: evidenceRoot"
        ),
        plan_issue=require_string(document["planIssue"], f"{path}: planIssue"),
    )


def verify(repo_root: Path, policy_path: Path) -> dict[str, object]:
    errors: list[str] = []
    try:
        policy = load_policy(policy_path, repo_root)
    except PolicyError as error:
        return {
            "schemaVersion": 1,
            "passed": False,
            "errors": [str(error)],
        }

    for relative in GOVERNANCE_PATHS:
        path = repo_root / relative
        if not path.is_file():
            errors.append(f"required governance file is missing: {relative}")
            continue
        classified = inventory.classify_path(relative)
        if classified["lane"] != "intentionally_owned":
            errors.append(
                f"{relative} is {classified['lane']}, expected intentionally_owned"
            )
        if "GOVERNANCE-1" not in classified["contracts"]:
            errors.append(f"{relative} is not covered by GOVERNANCE-1")

    contracts = repo_root / policy.contracts_path
    if not contracts.is_file():
        errors.append(f"contracts document is missing: {policy.contracts_path}")
    evidence = repo_root / policy.evidence_root
    if not evidence.is_dir():
        errors.append(f"evidence root is missing: {policy.evidence_root}")

    agents = (repo_root / "AGENTS.md").read_text(encoding="utf-8")
    if "$upstream-convergence" not in agents:
        errors.append("AGENTS.md does not route refresh work to $upstream-convergence")
    if policy.contracts_path not in agents:
        errors.append("AGENTS.md does not name the repository contract authority")

    readme = (repo_root / "upstream" / "README.md").read_text(encoding="utf-8")
    for expected in (
        "upstream/convergence-policy.json",
        policy.contracts_path,
        ".github/scripts/upstream_convergence.py",
    ):
        if expected not in readme:
            errors.append(f"upstream/README.md does not reference {expected}")

    repo_checks = (repo_root / ".github" / "workflows" / "repo-checks.yml").read_text(
        encoding="utf-8"
    )
    for command in (
        "python3 .github/scripts/verify_upstream_convergence_governance.py",
        "python3 .github/scripts/upstream_convergence_guard.py",
        "python3 .github/scripts/upstream_convergence.py validate",
        "test_upstream_convergence_*.py",
    ):
        if command not in repo_checks:
            errors.append(f"repo-checks.yml does not run {command}")

    blocking_ci = (
        repo_root / ".github" / "workflows" / "blocking-ci.yml"
    ).read_text(encoding="utf-8")
    if "uses: ./.github/workflows/repo-checks.yml" not in blocking_ci:
        errors.append("blocking-ci.yml does not call repo-checks.yml")

    return {
        "schemaVersion": 1,
        "repository": policy.repository,
        "policy": str(policy_path.relative_to(repo_root)),
        "requiredPaths": len(GOVERNANCE_PATHS),
        "errors": errors,
        "passed": not errors,
    }


def print_report(report: dict[str, object]) -> None:
    for error in report["errors"]:
        print(f"::error::{error}", file=sys.stderr)
    if report["passed"]:
        print(
            f"Upstream convergence governance passed for "
            f"{report['requiredPaths']} required files."
        )
    else:
        print(
            f"Upstream convergence governance failed with "
            f"{len(report['errors'])} errors.",
            file=sys.stderr,
        )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=str(REPO_ROOT))
    parser.add_argument("--policy", default=str(DEFAULT_POLICY))
    parser.add_argument("--json", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    repo_root = Path(args.repo_root).resolve()
    report = verify(repo_root, Path(args.policy).resolve())
    if args.json:
        json.dump(report, sys.stdout, indent=2, sort_keys=True)
        print()
    else:
        print_report(report)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
