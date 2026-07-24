#!/usr/bin/env python3

import re
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_REFERENCE = re.compile(
    r"^\s*uses:\s+\./\.github/workflows/(?P<name>[^#\s]+)\s*$",
    re.MULTILINE,
)
REPOSITORY_DERIVED_RUNNER = re.compile(
    r"\$\{\{\s*github\.event\.repository\.name\s*\}\}-"
    r"(?:runners|(?:linux|windows|macos)[A-Za-z0-9_-]*)"
)
SELF_HOSTED_RUNNER = re.compile(r"\bself-hosted\b")
MACOS_LARGE_RUNNER = re.compile(r"\bmacos-[0-9]+-(?:large|xlarge)\b")
RUNNER_SELECTOR = re.compile(
    r"^\s*(?:-\s*)?(?:runs-on|runner|os|group|labels)\s*:"
)
UNSUPPORTED_PLATFORM_ALIAS = re.compile(
    r"\b(?:windows-(?:x64|arm64)|linux-(?:x64|arm64)(?:-xl)?)\b"
)
RUNNER_BLOCK = re.compile(r"^(?:runs-on|runs_on)\s*:\s*$")
RUNNER_GROUP = re.compile(r"^group\s*:")
INLINE_RUNNER_GROUP = re.compile(r"^(?:runs-on|runs_on)\s*:.*\bgroup\s*:")


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    reason: str


def find_violations(
    workflows_dir: Path,
    entrypoint: str = "blocking-ci.yml",
) -> list[Violation]:
    pending = [workflows_dir / entrypoint]
    visited: set[Path] = set()
    violations: list[Violation] = []

    while pending:
        path = pending.pop()
        if path in visited:
            continue
        visited.add(path)
        if not path.is_file():
            violations.append(Violation(path, 0, "referenced workflow does not exist"))
            continue

        contents = path.read_text()
        pending.extend(
            workflows_dir / match.group("name")
            for match in WORKFLOW_REFERENCE.finditer(contents)
        )

        runner_block_indent: int | None = None
        for line_number, line in enumerate(contents.splitlines(), start=1):
            stripped = line.lstrip()
            indent = len(line) - len(stripped)
            if (
                runner_block_indent is not None
                and stripped
                and indent <= runner_block_indent
            ):
                runner_block_indent = None
            if RUNNER_BLOCK.match(stripped):
                runner_block_indent = indent
            elif INLINE_RUNNER_GROUP.match(stripped) or (
                runner_block_indent is not None
                and indent > runner_block_indent
                and RUNNER_GROUP.match(stripped)
            ):
                violations.append(
                    Violation(path, line_number, "runner group selector")
                )
            if REPOSITORY_DERIVED_RUNNER.search(line):
                violations.append(
                    Violation(path, line_number, "repository-derived runner selector")
                )
            if SELF_HOSTED_RUNNER.search(line):
                violations.append(
                    Violation(path, line_number, "persistent self-hosted runner")
                )
            if RUNNER_SELECTOR.search(line):
                if MACOS_LARGE_RUNNER.search(line):
                    violations.append(
                        Violation(path, line_number, "billable macOS large runner")
                    )
                if UNSUPPORTED_PLATFORM_ALIAS.search(line):
                    violations.append(
                        Violation(path, line_number, "unsupported platform runner alias")
                    )

    return sorted(
        violations,
        key=lambda violation: (str(violation.path), violation.line, violation.reason),
    )


def main() -> int:
    violations = find_violations(ROOT / ".github/workflows")
    if not violations:
        print("blocking CI runner routing is compatible with Codex Lab")
        return 0

    for violation in violations:
        path = violation.path.relative_to(ROOT)
        location = f"{path}:{violation.line}" if violation.line else str(path)
        print(f"{location}: {violation.reason}")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
