#!/usr/bin/env python3
"""Decide which extended Codex Lab checks apply to a change."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import fnmatch
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CONFIG = REPO_ROOT / ".github" / "extended-checks.json"


@dataclass(frozen=True)
class ExtendedCheck:
    name: str
    description: str
    workflow: str
    same_repo_only: bool
    patterns: tuple[str, ...]


@dataclass(frozen=True)
class CheckDecision:
    name: str
    description: str
    workflow: str
    required: bool
    available: bool
    matched_paths: tuple[str, ...]
    skip_reason: str | None
    unavailable_reason: str | None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--config",
        type=Path,
        default=DEFAULT_CONFIG,
        help="Routing config file (default: .github/extended-checks.json).",
    )
    parser.add_argument("--base", help="Base git revision for changed-file detection.")
    parser.add_argument("--head", help="Head git revision for changed-file detection.")
    parser.add_argument(
        "--event-name",
        default=os.environ.get("GITHUB_EVENT_NAME", "pull_request"),
        help="GitHub event name for the change being evaluated.",
    )
    parser.add_argument(
        "--changed-file",
        dest="changed_files",
        action="append",
        default=[],
        help="Changed file to evaluate. May be repeated; skips git diff when provided.",
    )
    parser.add_argument(
        "--changed-files-file",
        type=Path,
        help="File containing one changed path per line.",
    )
    parser.add_argument(
        "--head-repo",
        default=os.environ.get("PR_HEAD_REPO", os.environ.get("GITHUB_REPOSITORY", "")),
        help="Pull request head repository full name.",
    )
    parser.add_argument(
        "--base-repo",
        default=os.environ.get("GITHUB_REPOSITORY", ""),
        help="Base repository full name.",
    )
    parser.add_argument(
        "--format",
        choices=("json", "markdown"),
        default="json",
        help="Format to print to stdout (default: json).",
    )
    parser.add_argument(
        "--validate-config",
        action="store_true",
        help="Validate config and workflow coverage, then exit.",
    )
    return parser.parse_args()


def load_checks(config_path: Path) -> list[ExtendedCheck]:
    payload = json.loads(config_path.read_text())
    checks: list[ExtendedCheck] = []
    for name, raw_check in sorted(payload.get("checks", {}).items()):
        patterns = tuple(raw_check.get("patterns", []))
        if not patterns:
            raise ValueError(f"extended check {name} must define at least one pattern")
        checks.append(
            ExtendedCheck(
                name=name,
                description=raw_check.get("description", ""),
                workflow=raw_check["workflow"],
                same_repo_only=bool(raw_check.get("same_repo_only", False)),
                patterns=patterns,
            )
        )
    if not checks:
        raise ValueError(f"no extended checks defined in {config_path}")
    return checks


def changed_files_from_args(args: argparse.Namespace) -> list[str]:
    changed_files: list[str] = []
    changed_files.extend(args.changed_files)
    if args.changed_files_file is not None:
        changed_files.extend(args.changed_files_file.read_text().splitlines())
    if changed_files:
        return normalize_changed_files(changed_files)

    if not args.base or not args.head:
        raise ValueError(
            "provide --changed-file, --changed-files-file, or --base and --head"
        )

    return normalize_changed_files(git_changed_files(args.base, args.head))


def git_changed_files(base: str, head: str) -> list[str]:
    if is_all_zero_revision(base):
        command = all_zero_base_diff_command(head)
    else:
        command = ["git", "diff", "--name-only", f"{base}...{head}"]

    completed = subprocess.run(
        command,
        cwd=REPO_ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return completed.stdout.splitlines()


def is_all_zero_revision(revision: str) -> bool:
    return bool(revision) and set(revision) == {"0"}


def all_zero_base_diff_command(head: str) -> list[str]:
    merge_base = default_branch_merge_base(head)
    if merge_base:
        return ["git", "diff", "--name-only", f"{merge_base}...{head}"]
    return [
        "git",
        "diff-tree",
        "--root",
        "--no-commit-id",
        "--name-only",
        "-r",
        head,
    ]


def default_branch_merge_base(head: str) -> str | None:
    for candidate in ("origin/main", "main"):
        completed = subprocess.run(
            ["git", "merge-base", candidate, head],
            cwd=REPO_ROOT,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        merge_base = completed.stdout.strip()
        if completed.returncode == 0 and merge_base:
            return merge_base
    return None


def normalize_changed_files(paths: list[str]) -> list[str]:
    normalized: list[str] = []
    seen: set[str] = set()
    for path in paths:
        clean = path.strip().replace("\\", "/")
        if clean.startswith("./"):
            clean = clean[2:]
        if not clean or clean in seen:
            continue
        seen.add(clean)
        normalized.append(clean)
    return normalized


def decide_checks(
    checks: list[ExtendedCheck],
    changed_files: list[str],
    *,
    head_repo: str,
    base_repo: str,
    event_name: str = "pull_request",
) -> list[CheckDecision]:
    decisions: list[CheckDecision] = []
    same_repo = not head_repo or not base_repo or head_repo == base_repo
    for check in checks:
        if event_name != "pull_request":
            decisions.append(
                CheckDecision(
                    name=check.name,
                    description=check.description,
                    workflow=check.workflow,
                    required=False,
                    available=True,
                    matched_paths=(),
                    skip_reason=(
                        f"{check.workflow} runs automatically for pull_request changes; "
                        f"{event_name} does not trigger it."
                    ),
                    unavailable_reason=None,
                )
            )
            continue
        matched_paths = tuple(
            path
            for path in changed_files
            if any(matches_pattern(path, pattern) for pattern in check.patterns)
        )
        required = bool(matched_paths)
        available = not (required and check.same_repo_only and not same_repo)
        unavailable_reason = None
        if required and not available:
            unavailable_reason = (
                f"{check.name} requires self-hosted runners and cannot run automatically "
                "for fork pull requests"
            )
        decisions.append(
            CheckDecision(
                name=check.name,
                description=check.description,
                workflow=check.workflow,
                required=required,
                available=available,
                matched_paths=matched_paths,
                skip_reason=None,
                unavailable_reason=unavailable_reason,
            )
        )
    return decisions


def matches_pattern(path: str, pattern: str) -> bool:
    normalized_pattern = pattern.rstrip("/")
    if normalized_pattern.endswith("/**"):
        prefix = normalized_pattern[:-3].rstrip("/")
        return path == prefix or path.startswith(f"{prefix}/")
    return fnmatch.fnmatchcase(path, normalized_pattern)


def validate_config(checks: list[ExtendedCheck], config_path: Path) -> None:
    errors: list[str] = []
    tracked_files = repo_files()
    for check in checks:
        workflow_path = REPO_ROOT / check.workflow
        if not workflow_path.is_file():
            errors.append(f"{check.name}: workflow does not exist: {check.workflow}")
        workflow_text = workflow_path.read_text() if workflow_path.is_file() else ""
        workflow_paths = workflow_pull_request_paths(workflow_text)
        if not workflow_paths:
            errors.append(f"{check.name}: workflow has no pull_request.paths block")
        elif workflow_paths != check.patterns:
            errors.append(
                f"{check.name}: workflow pull_request.paths differ from extended-checks config\n"
                f"  workflow: {list(workflow_paths)}\n"
                f"  config:   {list(check.patterns)}"
            )
        for required_pattern in [
            relative_path(config_path),
            "scripts/github/decide_extended_checks.py",
            "scripts/github/test_decide_extended_checks.py",
            check.workflow,
        ]:
            if required_pattern not in check.patterns:
                errors.append(
                    f"{check.name}: missing self-maintenance pattern {required_pattern}"
                )
        for script_path in workflow_run_paths(workflow_text):
            if not any(
                matches_pattern(script_path, pattern) for pattern in check.patterns
            ):
                errors.append(
                    f"{check.name}: workflow references {script_path} but routing patterns do not cover it"
                )
        for pattern in check.patterns:
            if is_glob(pattern):
                if not any(matches_pattern(path, pattern) for path in tracked_files):
                    errors.append(
                        f"{check.name}: pattern matches no tracked files: {pattern}"
                    )
                continue
            if not (REPO_ROOT / pattern).exists():
                errors.append(f"{check.name}: path does not exist: {pattern}")
    if errors:
        raise ValueError("\n".join(errors))


def workflow_run_paths(workflow_text: str) -> set[str]:
    paths: set[str] = set()
    for line in workflow_text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        for token in stripped.replace("\\", " ").split():
            clean = token.strip("'\"")
            if clean.startswith("./"):
                clean = clean[2:]
            if clean.startswith("scripts/"):
                paths.add(clean)
    return paths


def workflow_pull_request_paths(workflow_text: str) -> tuple[str, ...]:
    lines = workflow_text.splitlines()
    for index, line in enumerate(lines):
        if line.strip() != "pull_request:":
            continue
        pull_request_indent = indentation(line)
        paths_index = find_child_key(lines, index + 1, pull_request_indent, "paths:")
        if paths_index is None:
            continue
        return tuple(
            parse_yaml_list(lines, paths_index + 1, indentation(lines[paths_index]))
        )
    return ()


def find_child_key(
    lines: list[str], start_index: int, parent_indent: int, key: str
) -> int | None:
    for index in range(start_index, len(lines)):
        line = lines[index]
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = indentation(line)
        if indent <= parent_indent:
            return None
        if stripped == key:
            return index
    return None


def parse_yaml_list(
    lines: list[str], start_index: int, parent_indent: int
) -> list[str]:
    values: list[str] = []
    for line in lines[start_index:]:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = indentation(line)
        if indent <= parent_indent:
            break
        if not stripped.startswith("- "):
            continue
        values.append(stripped[2:].strip().strip("\"'"))
    return values


def indentation(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def relative_path(path: Path) -> str:
    return path.resolve().relative_to(REPO_ROOT).as_posix()


def is_glob(pattern: str) -> bool:
    return any(char in pattern for char in "*?[")


def repo_files() -> list[str]:
    completed = subprocess.run(
        ["git", "ls-files"],
        cwd=REPO_ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return completed.stdout.splitlines()


def payload_for(
    decisions: list[CheckDecision], changed_files: list[str]
) -> dict[str, Any]:
    return {
        "changedFiles": changed_files,
        "checks": [
            {
                "name": decision.name,
                "description": decision.description,
                "workflow": decision.workflow,
                "required": decision.required,
                "available": decision.available,
                "matchedPaths": list(decision.matched_paths),
                "skipReason": decision.skip_reason,
                "unavailableReason": decision.unavailable_reason,
            }
            for decision in decisions
        ],
    }


def markdown_for(decisions: list[CheckDecision], changed_files: list[str]) -> str:
    lines = ["### Extended Checks Decision", ""]
    lines.append(f"Changed files considered: `{len(changed_files)}`")
    lines.extend(["", "| Check | Decision | Reason |", "| --- | --- | --- |"])
    for decision in decisions:
        if decision.required and decision.available:
            state = "required"
        elif decision.required:
            state = "manual required"
        else:
            state = "skipped"
        lines.append(f"| `{decision.name}` | {state} | {reason_for(decision)} |")
    return "\n".join(lines) + "\n"


def reason_for(decision: CheckDecision) -> str:
    if decision.skip_reason:
        return decision.skip_reason
    if decision.unavailable_reason:
        return decision.unavailable_reason
    if not decision.required:
        return "No changed files matched this check's routing patterns."
    preview = ", ".join(f"`{path}`" for path in decision.matched_paths[:5])
    remaining = len(decision.matched_paths) - 5
    if remaining > 0:
        preview += f", and {remaining} more"
    return preview


def main() -> int:
    args = parse_args()
    checks = load_checks(args.config)
    if args.validate_config:
        validate_config(checks, args.config)
        return 0

    changed_files = changed_files_from_args(args)
    decisions = decide_checks(
        checks,
        changed_files,
        head_repo=args.head_repo,
        base_repo=args.base_repo,
        event_name=args.event_name,
    )
    if args.format == "markdown":
        print(markdown_for(decisions, changed_files), end="")
    else:
        print(
            json.dumps(payload_for(decisions, changed_files), indent=2, sort_keys=True)
        )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
