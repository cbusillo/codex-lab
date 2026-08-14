#!/usr/bin/env python3

"""Verify release-critical Bazel edges for Rust compile-time file reads."""

import argparse
from dataclasses import dataclass
import fnmatch
import json
from pathlib import Path, PurePosixPath
import re
import sys


REPO_ROOT = Path(__file__).resolve().parents[2]
DIRECT_INCLUDE = re.compile(
    r"\b(?P<macro>include_str|include_bytes)!\s*\(\s*"
    r'"(?P<path>(?:\\.|[^"\\])*)"\s*\)',
    re.DOTALL,
)
ANY_INCLUDE = re.compile(r"\binclude_(?:str|bytes)!\s*\(")
SQLX_MIGRATE = re.compile(
    r"\bsqlx::migrate!\s*\(\s*"
    r'"(?P<path>(?:\\.|[^"\\])*)"\s*\)',
    re.DOTALL,
)
STRING_LITERAL = re.compile(r'"((?:\\.|[^"\\])*)"')
ALLOWED_DYNAMIC_INCLUDE_COUNTS = {
    "codex-rs/tui/src/frames.rs": 36,
}


@dataclass(frozen=True)
class Failure:
    source: str
    line: int
    message: str


def decode_string(value: str) -> str:
    return json.loads(f'"{value}"')


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def expression_end(text: str, start: int) -> int:
    brackets: list[str] = []
    string_quote: str | None = None
    escaped = False
    pairs = {"(": ")", "[": "]", "{": "}"}
    for index in range(start, len(text)):
        character = text[index]
        if string_quote is not None:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == string_quote:
                string_quote = None
            continue
        if character in {'"', "'"}:
            string_quote = character
        elif character in pairs:
            brackets.append(pairs[character])
        elif brackets and character == brackets[-1]:
            brackets.pop()
        elif not brackets and character in {",", "\n"}:
            return index
    return len(text)


def assignment_expression(text: str, name: str) -> str | None:
    match = re.search(rf"\b{re.escape(name)}\s*=", text)
    if match is None:
        return None
    start = match.end()
    return text[start : expression_end(text, start)].strip()


def string_values(expression: str) -> set[str]:
    return {decode_string(match.group(1)) for match in STRING_LITERAL.finditer(expression)}


def compile_data_values(build_text: str) -> set[str]:
    expression = assignment_expression(build_text, "compile_data")
    if expression is None:
        return set()
    values = string_values(expression)
    if values:
        return values
    alias = expression.strip()
    if re.fullmatch(r"[A-Z][A-Z0-9_]*", alias):
        alias_expression = assignment_expression(build_text, alias)
        if alias_expression is not None:
            return string_values(alias_expression)
    return set()


def call_expressions(text: str, name: str) -> list[str]:
    expressions: list[str] = []
    for match in re.finditer(rf"\b{re.escape(name)}\s*\(", text):
        start = match.end()
        end = expression_end(text, start)
        expressions.append(text[start:end])
    return expressions


def exported_files(build_text: str) -> set[str]:
    values: set[str] = set()
    for expression in call_expressions(build_text, "exports_files"):
        values.update(string_values(expression))
    return values


def nearest_bazel_package(path: Path, repo_root: Path) -> Path | None:
    current = path if path.is_dir() else path.parent
    root = repo_root.resolve()
    while True:
        if (current / "BUILD.bazel").is_file():
            return current
        if current == root:
            return None
        try:
            current.relative_to(root)
        except ValueError:
            return None
        current = current.parent


def bazel_label(repo_root: Path, package_root: Path, target: Path) -> str:
    package = package_root.relative_to(repo_root).as_posix()
    if package == ".":
        package = ""
    target_name = target.relative_to(package_root).as_posix()
    return f"//{package}:{target_name}" if package else f"//:{target_name}"


def pattern_covers(pattern: str, target: str, *, directory: bool) -> bool:
    if pattern.startswith("//"):
        return False
    candidates = [target]
    if directory:
        candidates.append(f"{target.rstrip('/')}/__migration__.sql")
    return any(fnmatch.fnmatchcase(candidate, pattern) for candidate in candidates)


def verify(repo_root: Path) -> dict[str, object]:
    root = repo_root.resolve()
    rust_root = root / "codex-rs"
    failures: list[Failure] = []
    include_count = 0
    cross_package_count = 0
    migration_count = 0

    for source in sorted(rust_root.rglob("*.rs")):
        if source.name == "build.rs":
            continue
        text = source.read_text(encoding="utf-8")
        source_relative = source.relative_to(root).as_posix()
        direct_positions: set[int] = set()

        for match in DIRECT_INCLUDE.finditer(text):
            direct_positions.add(match.start())
            include_count += 1
            requested = decode_string(match.group("path"))
            target = (source.parent / requested).resolve()
            line = line_number(text, match.start())
            if not target.is_file():
                failures.append(
                    Failure(source_relative, line, f"compile-time file is missing: {target}")
                )
                continue
            consumer = nearest_bazel_package(source, root)
            producer = nearest_bazel_package(target, root)
            if consumer is None or producer is None:
                failures.append(
                    Failure(source_relative, line, "cannot resolve Bazel package boundary")
                )
                continue
            if consumer == producer:
                continue
            cross_package_count += 1
            label = bazel_label(root, producer, target)
            consumer_build = (consumer / "BUILD.bazel").read_text(encoding="utf-8")
            if label not in compile_data_values(consumer_build):
                failures.append(
                    Failure(
                        source_relative,
                        line,
                        f"consumer BUILD.bazel is missing compile_data label {label}",
                    )
                )
            target_name = target.relative_to(producer).as_posix()
            producer_build = (producer / "BUILD.bazel").read_text(encoding="utf-8")
            if target_name not in exported_files(producer_build):
                failures.append(
                    Failure(
                        source_relative,
                        line,
                        f"producer BUILD.bazel does not export {target_name}",
                    )
                )

        dynamic_positions = [
            match.start()
            for match in ANY_INCLUDE.finditer(text)
            if match.start() not in direct_positions
        ]
        expected_dynamic_count = ALLOWED_DYNAMIC_INCLUDE_COUNTS.get(source_relative)
        if dynamic_positions and expected_dynamic_count != len(dynamic_positions):
            failures.append(
                Failure(
                    source_relative,
                    line_number(text, dynamic_positions[0]),
                    "unsupported non-literal include macros: expected "
                    f"{expected_dynamic_count or 0}, found {len(dynamic_positions)}",
                )
            )

        for match in SQLX_MIGRATE.finditer(text):
            migration_count += 1
            line = line_number(text, match.start())
            consumer = nearest_bazel_package(source, root)
            if consumer is None:
                failures.append(
                    Failure(source_relative, line, "cannot resolve Bazel package boundary")
                )
                continue
            requested = decode_string(match.group("path"))
            target = (consumer / requested).resolve()
            if not target.is_dir():
                failures.append(
                    Failure(source_relative, line, f"migration directory is missing: {target}")
                )
                continue
            target_name = target.relative_to(consumer).as_posix()
            build_text = (consumer / "BUILD.bazel").read_text(encoding="utf-8")
            if not any(
                pattern_covers(pattern, target_name, directory=True)
                for pattern in compile_data_values(build_text)
            ):
                failures.append(
                    Failure(
                        source_relative,
                        line,
                        f"consumer BUILD.bazel does not cover migration directory {target_name}",
                    )
                )

    errors = [
        f"{failure.source}:{failure.line}: {failure.message}"
        for failure in sorted(failures, key=lambda item: (item.source, item.line, item.message))
    ]
    return {
        "schemaVersion": 1,
        "includeSites": include_count,
        "crossPackageEdges": cross_package_count,
        "migrationEdges": migration_count,
        "errors": errors,
        "passed": not errors,
    }


def print_report(report: dict[str, object]) -> None:
    for error in report["errors"]:
        print(f"::error::{error}", file=sys.stderr)
    if report["passed"]:
        print(
            "Bazel compile-data verification passed for "
            f"{report['crossPackageEdges']} cross-package edges and "
            f"{report['migrationEdges']} migration edges."
        )
    else:
        print(
            f"Bazel compile-data verification failed with {len(report['errors'])} errors.",
            file=sys.stderr,
        )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=str(REPO_ROOT))
    parser.add_argument("--json", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    report = verify(Path(args.repo_root))
    if args.json:
        json.dump(report, sys.stdout, indent=2, sort_keys=True)
        print()
    else:
        print_report(report)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
