#!/usr/bin/env python3

"""Fail when a Python test file in a repo-checks test directory never runs.

`repo-checks.yml` used to name each `.github/scripts/test_*.py` file in its own
`unittest discover -p '<exact file>'` step. Adding a test file then required
remembering to add a step, and forgetting was invisible: the suite sat in the
tree looking like coverage while CI never executed it. Two files
(`test_rusty_v8_bazel.py` and `test_run_bazel_with_buildbuddy.py`) were
unregistered that way.

The workflow now discovers whole directories, and this verifier keeps that
property honest: for every `unittest discover -s <dir> -p '<pattern>'` the
workflow runs, every `test_*.py` under `<dir>` must match one of the patterns
registered for that directory. It intentionally only inspects directories the
workflow already discovers, so Python suites owned by other workflows (the
Python SDK, Bazel lint tooling, agent skills) stay out of scope.
"""

import argparse
import fnmatch
import re
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "repo-checks.yml"

TEST_GLOB = "test_*.py"

# `python3 -m unittest discover -s <dir> -p '<pattern>'`. Shell line
# continuations are folded away before matching, so a wrapped invocation reads
# the same as a single-line one.
DISCOVER = re.compile(
    r"unittest\s+discover\s+-s\s+(?P<directory>\S+)\s+-p\s+"
    r"(?P<pattern>'[^']+'|\"[^\"]+\"|\S+)"
)


def discovery_registrations(workflow_text: str) -> dict[str, list[str]]:
    """Map each discovered directory to the patterns the workflow runs for it."""

    joined = re.sub(r"\\\n\s*", " ", workflow_text)
    registrations: dict[str, list[str]] = {}
    for match in DISCOVER.finditer(joined):
        pattern = match.group("pattern").strip("'\"")
        registrations.setdefault(match.group("directory"), []).append(pattern)
    return registrations


def unregistered_tests(
    registrations: dict[str, list[str]], repo_root: Path
) -> list[tuple[str, str]]:
    """Return `(path, detail)` for every test file the workflow would not run."""

    problems: list[tuple[str, str]] = []
    repo_root = repo_root.resolve()
    discovery_roots = {
        directory: (repo_root / directory).resolve() for directory in registrations
    }
    for directory, patterns in sorted(registrations.items()):
        base = discovery_roots[directory]
        if not base.is_dir():
            problems.append((directory, "discovery directory does not exist"))
            continue
        for test_file in sorted(base.rglob(TEST_GLOB)):
            if any(
                other_base != base and test_file.is_relative_to(other_base)
                for other_base in discovery_roots.values()
            ):
                continue
            relative = test_file.relative_to(repo_root).as_posix()
            if not any(
                fnmatch.fnmatchcase(test_file.name, pattern) for pattern in patterns
            ):
                registered = ", ".join(patterns)
                problems.append(
                    (
                        relative,
                        f"no discovery pattern for {directory} matches it "
                        f"(registered: {registered})",
                    )
                )
                continue
            # `unittest discover` only walks into importable subdirectories, so a
            # matching name inside a plain directory silently never runs.
            if test_file.parent != base and not (test_file.parent / "__init__.py").is_file():
                problems.append(
                    (
                        relative,
                        "nested test directory needs __init__.py for unittest "
                        "discovery to reach it",
                    )
                )
    return problems


def is_registered(test_file_name: str, directory: str, workflow_text: str) -> bool:
    """Whether the workflow discovers `test_file_name` inside `directory`."""

    patterns = discovery_registrations(workflow_text).get(directory, [])
    return any(fnmatch.fnmatchcase(test_file_name, pattern) for pattern in patterns)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workflow", default=str(DEFAULT_WORKFLOW))
    parser.add_argument("--repo-root", default=str(REPO_ROOT))
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    workflow = Path(args.workflow)
    try:
        workflow_text = workflow.read_text(encoding="utf-8")
    except OSError as error:
        print(f"::error::cannot read {workflow}: {error}", file=sys.stderr)
        return 2

    registrations = discovery_registrations(workflow_text)
    if not registrations:
        print(
            f"::error file={workflow}::no unittest discovery steps found; "
            "repo-check Python tests would not run at all",
            file=sys.stderr,
        )
        return 2

    problems = unregistered_tests(registrations, Path(args.repo_root).resolve())
    print(f"Discovered test directories: {len(registrations)}")
    for path, detail in problems:
        print(f"::error file={path}::unregistered test: {detail}", file=sys.stderr)
    if problems:
        print(
            f"repo-checks test registration failed: {len(problems)} test files "
            "would never run.",
            file=sys.stderr,
        )
        return 1
    print("Every repo-check Python test file is registered.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
