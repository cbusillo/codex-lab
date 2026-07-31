#!/usr/bin/env python3

import argparse
import json
import re
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_REFERENCE = re.compile(
    r"^\s*uses:\s+\./\.github/workflows/(?P<name>[^#\s]+)\s*$",
    re.MULTILINE,
)
SELECTOR = re.compile(
    r"^(?P<indent>\s*)(?:-\s*)?"
    r"(?P<key>runs-on|runs_on|runner|target|platform|image)\s*:\s*"
    r"(?P<value>.*?)\s*$"
)
BLOCK_SCALAR = re.compile(r"^\s*[A-Za-z0-9_-]+:\s*[>|][0-9+-]*\s*$")
EXPRESSION = re.compile(r"^\$\{\{.*\}\}$")
APPLE_RUNNERS = frozenset({"macos-26", "macos-codex-lab"})
APPLE_TARGET = "aarch64-apple-darwin"
APPLE_PLATFORMS = frozenset({"macos_arm64", "macos-aarch64"})
OUT_OF_SCOPE_ENTRYPOINTS = frozenset(
    {
        "cla.yml",
        "close-stale-contributor-prs.yml",
        "issue-deduplicator.yml",
        "issue-labeler.yml",
        "issue-translator.yml",
    }
)
PAUSED_NON_APPLE_WORKFLOWS = frozenset(
    {
        "python-runtime-build.yml",
        "python-runtime-release.yml",
        "python-sdk-release.yml",
        "rust-ci-full-windows.yml",
        "rust-release-argument-comment-lint.yml",
        "rust-release-windows.yml",
        "rust-release.yml",
        "v8-canary-windows.yml",
    }
)


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    reason: str


def workflow_paths(workflows_dir: Path) -> list[Path]:
    return sorted({*workflows_dir.glob("*.yml"), *workflows_dir.glob("*.yaml")})


def workflow_triggers(contents: str) -> set[str]:
    lines = contents.splitlines()
    for index, line in enumerate(lines):
        if not line.startswith("on:"):
            continue
        inline = line.removeprefix("on:").strip()
        if inline:
            return {
                value.strip().strip("'\"")
                for value in inline.strip("[]").split(",")
                if value.strip()
            }

        triggers: set[str] = set()
        for child in lines[index + 1 :]:
            stripped = child.lstrip()
            if not stripped or stripped.startswith("#"):
                continue
            indent = len(child) - len(stripped)
            if indent == 0:
                break
            if indent != 2:
                continue
            key, separator, _ = stripped.partition(":")
            if separator:
                triggers.add(key)
        return triggers
    return set()


def called_workflows(contents: str) -> set[str]:
    return {match.group("name") for match in WORKFLOW_REFERENCE.finditer(contents)}


def active_workflows(
    workflows_dir: Path,
    *,
    out_of_scope_entrypoints: frozenset[str] = OUT_OF_SCOPE_ENTRYPOINTS,
) -> tuple[set[Path], list[Violation]]:
    paths = workflow_paths(workflows_dir)
    by_name = {path.name: path for path in paths}
    roots = {
        path
        for path in paths
        if path.name not in out_of_scope_entrypoints
        and workflow_triggers(path.read_text()) - {"workflow_call"}
    }
    pending = list(roots)
    reachable: set[Path] = set()
    violations: list[Violation] = []

    while pending:
        path = pending.pop()
        if path in reachable:
            continue
        reachable.add(path)
        for name in called_workflows(path.read_text()):
            callee = by_name.get(name)
            if callee is None:
                violations.append(
                    Violation(path, 0, f"referenced workflow does not exist: {name}")
                )
                continue
            pending.append(callee)

    return reachable, violations


def clean_scalar(value: str) -> str:
    value = value.split(" #", maxsplit=1)[0].strip()
    return value.strip("'\"")


def validate_runner_value(path: Path, line: int, value: str) -> list[Violation]:
    value = clean_scalar(value)
    if EXPRESSION.fullmatch(value):
        return []
    if value.startswith("[") and value.endswith("]"):
        labels = {clean_scalar(label).lower() for label in value[1:-1].split(",")}
        if "macos" in labels and "arm64" in labels:
            return []
    elif value in APPLE_RUNNERS:
        return []
    return [Violation(path, line, f"non-Apple-Silicon runner selector: {value}")]


def selector_violations(path: Path, contents: str) -> list[Violation]:
    lines = contents.splitlines()
    violations: list[Violation] = []
    block_scalar_indent: int | None = None

    for index, line in enumerate(lines):
        line_number = index + 1
        stripped = line.lstrip()
        indent = len(line) - len(stripped)
        if block_scalar_indent is not None:
            if not stripped or indent > block_scalar_indent:
                continue
            block_scalar_indent = None
        if BLOCK_SCALAR.match(line):
            block_scalar_indent = indent
            continue
        if stripped.startswith("#"):
            continue
        if re.match(r"^\s*container\s*:", line):
            violations.append(
                Violation(path, line_number, "container jobs are not macOS execution")
            )
            continue

        match = SELECTOR.match(line)
        if match is None:
            continue
        key = match.group("key")
        value = match.group("value")
        if key in {"runs-on", "runs_on"} and not value:
            labels: list[str] = []
            for child in lines[index + 1 :]:
                child_stripped = child.lstrip()
                if not child_stripped:
                    continue
                child_indent = len(child) - len(child_stripped)
                if child_indent <= indent:
                    break
                if child_stripped.startswith("- "):
                    labels.append(clean_scalar(child_stripped.removeprefix("- ")))
            normalized = {label.lower() for label in labels}
            if "macos" not in normalized or "arm64" not in normalized:
                violations.append(
                    Violation(
                        path,
                        line_number,
                        f"non-Apple-Silicon runner labels: {', '.join(labels)}",
                    )
                )
            continue
        if not value:
            continue

        if key in {"runs-on", "runs_on", "runner"}:
            violations.extend(validate_runner_value(path, line_number, value))
            continue

        value = clean_scalar(value)
        if EXPRESSION.fullmatch(value):
            continue
        if key == "target" and value != APPLE_TARGET:
            violations.append(
                Violation(path, line_number, f"non-Apple-Silicon target: {value}")
            )
        elif key == "platform" and value not in APPLE_PLATFORMS:
            violations.append(
                Violation(path, line_number, f"non-Apple-Silicon platform: {value}")
            )
        elif key == "image":
            violations.append(
                Violation(path, line_number, f"container image is not macOS: {value}")
            )

    return violations


def repository_violations(root: Path) -> list[Violation]:
    workflows_dir = root / ".github/workflows"
    active, violations = active_workflows(workflows_dir)
    for path in sorted(active):
        violations.extend(selector_violations(path, path.read_text()))

    active_names = {path.name for path in active}
    for name in sorted(PAUSED_NON_APPLE_WORKFLOWS):
        path = workflows_dir / name
        if not path.is_file():
            violations.append(Violation(path, 0, "paused workflow is missing"))
            continue
        triggers = workflow_triggers(path.read_text())
        if triggers != {"workflow_call"}:
            violations.append(
                Violation(
                    path,
                    0,
                    "paused workflow must expose only workflow_call",
                )
            )
        if name in active_names:
            violations.append(Violation(path, 0, "paused workflow is still reachable"))

    dotslash_config = root / ".github/dotslash-zsh-config.json"
    platforms = (
        json.loads(dotslash_config.read_text())
        .get("outputs", {})
        .get("codex-zsh", {})
        .get("platforms", {})
    )
    if set(platforms) != {"macos-aarch64"}:
        violations.append(
            Violation(
                dotslash_config,
                0,
                "zsh release manifest must publish only macos-aarch64",
            )
        )

    return sorted(
        violations,
        key=lambda violation: (str(violation.path), violation.line, violation.reason),
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    violations = repository_violations(args.root.resolve())
    if not violations:
        print("active development and release workflows are Apple Silicon-only")
        return 0

    for violation in violations:
        try:
            path = violation.path.relative_to(args.root.resolve())
        except ValueError:
            path = violation.path
        location = f"{path}:{violation.line}" if violation.line else str(path)
        print(f"{location}: {violation.reason}")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
