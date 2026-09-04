#!/usr/bin/env python3

"""Verify the repository-owned upstream convergence control plane."""

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

import upstream_convergence_inventory as inventory
import upstream_convergence_guard as guard


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_POLICY = REPO_ROOT / "upstream" / "convergence-policy.json"
SUPPORTED_POLICY_SCHEMA = 1
MAX_POLICY_BYTES = 1024 * 1024
MAX_GOVERNANCE_FILE_BYTES = 4 * 1024 * 1024

GOVERNANCE_PATHS = (
    "AGENTS.md",
    "upstream/README.md",
    "upstream/convergence-contracts.md",
    "upstream/convergence-gates.json",
    "upstream/convergence-policy.json",
    "upstream/convergence-guard.json",
    "upstream/convergence-waivers.json",
    ".github/CODEOWNERS",
    ".github/scripts/upstream_convergence.py",
    ".github/scripts/upstream_convergence_guard.py",
    ".github/scripts/upstream_convergence_inventory.py",
    ".github/scripts/upstream_convergence_bazel_data.py",
    ".github/scripts/upstream_convergence_gates.py",
    ".github/scripts/verify_upstream_convergence_governance.py",
    ".github/scripts/test_upstream_convergence_driver.py",
    ".github/scripts/test_upstream_convergence_guard.py",
    ".github/scripts/test_upstream_convergence_inventory.py",
    ".github/scripts/test_upstream_convergence_bazel_data.py",
    ".github/scripts/test_upstream_convergence_gates.py",
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


EXPECTED_POLICY = ConvergencePolicy(
    repository="openai/codex",
    remote="openai",
    branch="main",
    allowed_fetch_urls=(
        "https://github.com/openai/codex.git",
        "git@github.com:openai/codex.git",
    ),
    contracts_path="upstream/convergence-contracts.md",
    evidence_root="upstream/openai-codex",
    plan_issue="https://github.com/cbusillo/codex-lab/issues/230",
)


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
    root = repo_root.resolve()
    resolved_policy = path.resolve()
    try:
        resolved_policy.relative_to(root)
    except ValueError as error:
        raise PolicyError(
            f"policy path escapes the repository: {resolved_policy}"
        ) from error
    try:
        if resolved_policy.stat().st_size > MAX_POLICY_BYTES:
            raise PolicyError(
                f"policy exceeds {MAX_POLICY_BYTES} bytes: {resolved_policy}"
            )
        document = json.loads(resolved_policy.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise PolicyError(f"missing convergence policy: {path}") from error
    except json.JSONDecodeError as error:
        raise PolicyError(f"invalid JSON in {path}: {error}") from error
    except (OSError, UnicodeError) as error:
        raise PolicyError(f"cannot read {path}: {error}") from error
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


def read_governance_text(path: Path, errors: list[str]) -> str | None:
    try:
        if path.stat().st_size > MAX_GOVERNANCE_FILE_BYTES:
            errors.append(
                f"governance file exceeds {MAX_GOVERNANCE_FILE_BYTES} bytes: {path}"
            )
            return None
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        errors.append(f"cannot read governance file {path}: {error}")
        return None


def guard_entry_without_baseline_blob(entry: dict[str, object]) -> dict[str, object]:
    return {key: value for key, value in entry.items() if key != "baselineBlob"}


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
    if policy != EXPECTED_POLICY:
        errors.append("convergence policy differs from the pinned Codex Lab identity")

    for relative in GOVERNANCE_PATHS:
        path = repo_root / relative
        if not path.is_file():
            errors.append(f"required governance file is missing: {relative}")
            continue
        if path.is_symlink():
            errors.append(f"required governance file must not be a symlink: {relative}")
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

    agents_path = repo_root / "AGENTS.md"
    if agents_path.is_file() and not agents_path.is_symlink():
        agents = read_governance_text(agents_path, errors)
        if agents is not None:
            if "$upstream-convergence" not in agents:
                errors.append(
                    "AGENTS.md does not route refresh work to $upstream-convergence"
                )
            if policy.contracts_path not in agents:
                errors.append(
                    "AGENTS.md does not name the repository contract authority"
                )

    readme_path = repo_root / "upstream" / "README.md"
    if readme_path.is_file() and not readme_path.is_symlink():
        readme = read_governance_text(readme_path, errors)
        if readme is not None:
            for expected in (
                "upstream/convergence-policy.json",
                policy.contracts_path,
                ".github/scripts/upstream_convergence.py",
            ):
                if expected not in readme:
                    errors.append(f"upstream/README.md does not reference {expected}")

    repo_checks_path = repo_root / ".github" / "workflows" / "repo-checks.yml"
    if repo_checks_path.is_file() and not repo_checks_path.is_symlink():
        repo_checks = read_governance_text(repo_checks_path, errors)
        if repo_checks is not None:
            for command in (
                "python3 .github/scripts/verify_upstream_convergence_governance.py",
                "python3 .github/scripts/upstream_convergence_gates.py",
                "python3 .github/scripts/upstream_convergence_bazel_data.py",
                "python3 .github/scripts/upstream_convergence_guard.py",
                "python3 .github/scripts/verify_repo_checks_test_registration.py",
                "python3 .github/scripts/upstream_convergence.py validate",
            ):
                if command not in repo_checks:
                    errors.append(f"repo-checks.yml does not run {command}")

    blocking_ci_path = repo_root / ".github" / "workflows" / "blocking-ci.yml"
    if blocking_ci_path.is_file() and not blocking_ci_path.is_symlink():
        blocking_ci = read_governance_text(blocking_ci_path, errors)
        if blocking_ci is not None:
            if "uses: ./.github/workflows/repo-checks.yml" not in blocking_ci:
                errors.append("blocking-ci.yml does not call repo-checks.yml")

    codeowners_path = repo_root / ".github" / "CODEOWNERS"
    if codeowners_path.is_file() and not codeowners_path.is_symlink():
        codeowners = read_governance_text(codeowners_path, errors)
        if codeowners is not None:
            for expected in (
                "/AGENTS.md @cbusillo",
                "/upstream/ @cbusillo",
                "/.github/CODEOWNERS @cbusillo",
                "/.github/workflows/repo-checks.yml @cbusillo",
            ):
                if expected not in codeowners:
                    errors.append(f"CODEOWNERS does not protect {expected.split()[0]}")

    guard_reproduced = False
    guard_path = repo_root / "upstream" / "convergence-guard.json"
    if guard_path.is_file() and not guard_path.is_symlink():
        try:
            checked_document = guard.load_json(guard_path)
            checked_entries = guard.load_manifest(guard_path)
            ownership_baseline = checked_document["ownershipBaseline"]
            if not isinstance(ownership_baseline, dict):
                raise ValueError("ownershipBaseline must be an object")
            recorded_current = str(ownership_baseline.get("current", ""))
            if guard.OBJECT_ID.fullmatch(recorded_current) is None:
                raise ValueError("ownershipBaseline.current must be a commit ID")
            head = inventory.resolve_commit(repo_root, "HEAD")
            if (
                inventory.run_git(
                    repo_root,
                    "merge-base",
                    "--is-ancestor",
                    recorded_current,
                    head,
                    check=False,
                ).returncode
                != 0
            ):
                raise ValueError("ownershipBaseline.current is not an ancestor of HEAD")
            expected_guard = inventory.build_guard_manifest(
                repo_root,
                guard.EXPECTED_OWNERSHIP_BASELINE["base"],
                guard.EXPECTED_OWNERSHIP_BASELINE["upstream"],
                guard.EXPECTED_OWNERSHIP_BASELINE["local"],
                head,
                inventory.LEGACY_POLICY_VERSION,
            )
            expected_entries = expected_guard["guardedPaths"]
            checked_baseline = [
                entry
                for entry in checked_entries
                if entry.get("source") == "ownership_baseline"
            ]
            expected_baseline = [
                entry
                for entry in expected_entries
                if entry.get("source") == "ownership_baseline"
            ]
            checked_current = [
                guard_entry_without_baseline_blob(entry)
                for entry in checked_entries
                if entry.get("source") == "current_tree"
            ]
            expected_current = [
                guard_entry_without_baseline_blob(entry)
                for entry in expected_entries
                if entry.get("source") == "current_tree"
            ]
            guard_reproduced = (
                checked_baseline == expected_baseline
                and checked_current == expected_current
            )
            if not guard_reproduced:
                errors.append(
                    "convergence guard does not reproduce from its immutable baseline "
                    "and current-tree path set"
                )
        except (
            KeyError,
            OSError,
            RuntimeError,
            ValueError,
            subprocess.SubprocessError,
        ) as error:
            errors.append(f"cannot reproduce convergence guard baseline: {error}")

    return {
        "schemaVersion": 1,
        "repository": policy.repository,
        "policy": str(policy_path.resolve().relative_to(repo_root.resolve())),
        "requiredPaths": len(GOVERNANCE_PATHS),
        "guardBaselineReproduced": guard_reproduced,
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
