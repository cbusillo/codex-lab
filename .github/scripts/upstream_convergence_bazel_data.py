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
    r"\b(?P<macro>include_str|include_bytes)!\s*"
    r'(?P<open>[({[])\s*"(?P<path>(?:\\.|[^"\\])*)"\s*(?P<close>[)}\]])',
    re.DOTALL,
)
ANY_INCLUDE = re.compile(r"\binclude_(?:str|bytes)!\s*[({[]")
SQLX_MIGRATE = re.compile(
    r"\bsqlx::migrate!\s*\(\s*"
    r'"(?P<path>(?:\\.|[^"\\])*)"\s*\)',
    re.DOTALL,
)
STRING_LITERAL = re.compile(r'"((?:\\.|[^"\\])*)"')
RAW_STRING_START = re.compile(r'(?:br|r)(?P<hashes>#{0,255})"')
CHAR_LITERAL = re.compile(r"'(?:\\.|[^\\'])'")
DELIMITER_PAIRS = {"(": ")", "{": "}", "[": "]"}
ALLOWED_DYNAMIC_INCLUDE_COUNTS = {
    "codex-rs/tui/src/frames.rs": 36,
}


@dataclass(frozen=True)
class Failure:
    source: str
    line: int
    message: str


@dataclass(frozen=True)
class DataSpec:
    values: frozenset[str]
    excludes: frozenset[str]


def decode_string(value: str) -> str:
    value = re.sub(
        r"\\u\{([0-9a-fA-F]{1,6})\}",
        lambda match: chr(int(match.group(1), 16)),
        value,
    )
    value = re.sub(
        r"\\x([0-9a-fA-F]{2})",
        lambda match: chr(int(match.group(1), 16)),
        value,
    )
    value = value.replace("\\'", "'")
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


def assignment_expressions(text: str, name: str) -> list[str]:
    expressions: list[str] = []
    for match in re.finditer(rf"\b{re.escape(name)}\s*=", text):
        start = match.end()
        expressions.append(text[start : expression_end(text, start)].strip())
    return expressions


def string_values(expression: str) -> set[str]:
    return {
        decode_string(match.group(1)) for match in STRING_LITERAL.finditer(expression)
    }


def keyword_argument_expressions(text: str, name: str) -> list[tuple[int, int, str]]:
    expressions: list[tuple[int, int, str]] = []
    for match in re.finditer(rf"\b{re.escape(name)}\s*=", text):
        start = match.end()
        end = expression_end(text, start)
        expressions.append((match.start(), end, text[start:end].strip()))
    return expressions


def without_keyword_arguments(text: str, name: str) -> str:
    result = text
    for start, end, _expression in reversed(keyword_argument_expressions(text, name)):
        while end < len(result) and result[end] in {",", " ", "\t"}:
            end += 1
        result = result[:start] + result[end:]
    return result


def compile_data_spec(build_text: str) -> DataSpec:
    build_text = mask_starlark_comments(build_text)
    values: set[str] = set()
    excludes: set[str] = set()
    pending = assignment_expressions(build_text, "compile_data")
    seen_aliases: set[str] = set()
    while pending:
        expression = pending.pop()
        excludes.update(
            value
            for _start, _end, exclude_expression in keyword_argument_expressions(
                expression, "exclude"
            )
            for value in string_values(exclude_expression)
        )
        values.update(string_values(without_keyword_arguments(expression, "exclude")))
        alias = expression.strip()
        if re.fullmatch(r"[A-Z][A-Z0-9_]*", alias) and alias not in seen_aliases:
            seen_aliases.add(alias)
            pending.extend(assignment_expressions(build_text, alias))
    return DataSpec(frozenset(values), frozenset(excludes))


def call_expressions(text: str, name: str) -> list[str]:
    expressions: list[str] = []
    for match in re.finditer(rf"\b{re.escape(name)}\s*\(", text):
        start = match.end() - 1
        end = expression_end(text, start)
        expressions.append(text[start:end])
    return expressions


def exported_file_spec(build_text: str) -> DataSpec:
    build_text = mask_starlark_comments(build_text)
    values: set[str] = set()
    excludes: set[str] = set()
    for expression in call_expressions(build_text, "exports_files"):
        excludes.update(
            value
            for _start, _end, exclude_expression in keyword_argument_expressions(
                expression, "exclude"
            )
            for value in string_values(exclude_expression)
        )
        values.update(string_values(without_keyword_arguments(expression, "exclude")))
    return DataSpec(frozenset(values), frozenset(excludes))


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


def nearest_cargo_package(path: Path, repo_root: Path) -> Path | None:
    current = path if path.is_dir() else path.parent
    root = repo_root.resolve()
    while True:
        if (current / "Cargo.toml").is_file():
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


def mask_comments_and_raw_strings(text: str) -> str:
    masked = list(text)
    index = 0
    block_depth = 0
    quote: str | None = None
    escaped = False
    while index < len(text):
        if block_depth:
            if text.startswith("/*", index):
                block_depth += 1
                masked[index : index + 2] = "  "
                index += 2
            elif text.startswith("*/", index):
                block_depth -= 1
                masked[index : index + 2] = "  "
                index += 2
            else:
                if text[index] != "\n":
                    masked[index] = " "
                index += 1
            continue
        if quote is not None:
            if escaped:
                escaped = False
            elif text[index] == "\\":
                escaped = True
            elif text[index] == quote:
                quote = None
            index += 1
            continue
        raw_match = (
            RAW_STRING_START.match(text, index) if text[index] in {"b", "r"} else None
        )
        if raw_match is not None:
            terminator = '"' + raw_match.group("hashes")
            end = text.find(terminator, index + raw_match.end())
            end = len(text) if end < 0 else end + len(terminator)
            for position in range(index, end):
                if text[position] != "\n":
                    masked[position] = " "
            index = end
        elif text.startswith("//", index):
            end = text.find("\n", index)
            end = len(text) if end < 0 else end
            masked[index:end] = " " * (end - index)
            index = end
        elif text.startswith("/*", index):
            block_depth = 1
            masked[index : index + 2] = "  "
            index += 2
        elif (char_match := CHAR_LITERAL.match(text, index)) is not None:
            index = char_match.end()
        elif text[index] == '"':
            quote = text[index]
            index += 1
        else:
            index += 1
    return "".join(masked)


def mask_starlark_comments(text: str) -> str:
    masked = list(text)
    quote: str | None = None
    escaped = False
    index = 0
    while index < len(text):
        character = text[index]
        if quote is not None:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == quote:
                quote = None
            index += 1
        elif character in {'"', "'"}:
            quote = character
            index += 1
        elif character == "#":
            end = text.find("\n", index)
            end = len(text) if end < 0 else end
            masked[index:end] = " " * (end - index)
            index = end
        else:
            index += 1
    return "".join(masked)


def verify(repo_root: Path) -> dict[str, object]:
    root = repo_root.resolve()
    rust_root = root / "codex-rs"
    failures: list[Failure] = []
    include_count = 0
    cross_package_count = 0
    migration_count = 0
    observed_dynamic_counts: dict[str, int] = {}

    for source in sorted(rust_root.rglob("*.rs")):
        relative_parts = source.relative_to(rust_root).parts
        if any(
            part == "target" or part.startswith("target-") for part in relative_parts
        ):
            continue
        if source.name == "build.rs":
            continue
        raw_text = source.read_text(encoding="utf-8")
        text = mask_comments_and_raw_strings(raw_text)
        source_relative = source.relative_to(root).as_posix()
        direct_positions: set[int] = set()

        for match in DIRECT_INCLUDE.finditer(text):
            direct_positions.add(match.start())
            include_count += 1
            if DELIMITER_PAIRS[match.group("open")] != match.group("close"):
                failures.append(
                    Failure(
                        source_relative,
                        line_number(text, match.start()),
                        "include macro uses mismatched delimiters",
                    )
                )
                continue
            try:
                requested = decode_string(match.group("path"))
            except (json.JSONDecodeError, ValueError) as error:
                failures.append(
                    Failure(
                        source_relative,
                        line_number(text, match.start()),
                        f"invalid Rust path literal: {error}",
                    )
                )
                continue
            target = (source.parent / requested).resolve()
            line = line_number(text, match.start())
            if not target.is_file():
                failures.append(
                    Failure(
                        source_relative, line, f"compile-time file is missing: {target}"
                    )
                )
                continue
            consumer = nearest_bazel_package(source, root)
            producer = nearest_bazel_package(target, root)
            if consumer is None or producer is None:
                failures.append(
                    Failure(
                        source_relative, line, "cannot resolve Bazel package boundary"
                    )
                )
                continue
            if consumer == producer:
                continue
            cross_package_count += 1
            label = bazel_label(root, producer, target)
            consumer_build = (consumer / "BUILD.bazel").read_text(encoding="utf-8")
            if label not in compile_data_spec(consumer_build).values:
                failures.append(
                    Failure(
                        source_relative,
                        line,
                        f"consumer BUILD.bazel is missing compile_data label {label}",
                    )
                )
            target_name = target.relative_to(producer).as_posix()
            producer_build = (producer / "BUILD.bazel").read_text(encoding="utf-8")
            export_spec = exported_file_spec(producer_build)
            exported = any(
                pattern_covers(pattern, target_name, directory=False)
                for pattern in export_spec.values
            ) and not any(
                pattern_covers(pattern, target_name, directory=False)
                for pattern in export_spec.excludes
            )
            if not exported:
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
        if expected_dynamic_count is not None:
            observed_dynamic_counts[source_relative] = len(dynamic_positions)
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
            cargo_package = nearest_cargo_package(source, root)
            if consumer is None or cargo_package is None:
                failures.append(
                    Failure(
                        source_relative,
                        line,
                        "cannot resolve Cargo and Bazel package boundaries",
                    )
                )
                continue
            try:
                requested = decode_string(match.group("path"))
            except (json.JSONDecodeError, ValueError) as error:
                failures.append(
                    Failure(
                        source_relative, line, f"invalid Rust path literal: {error}"
                    )
                )
                continue
            target = (cargo_package / requested).resolve()
            if not target.is_dir():
                failures.append(
                    Failure(
                        source_relative,
                        line,
                        f"migration directory is missing: {target}",
                    )
                )
                continue
            try:
                target_name = target.relative_to(consumer).as_posix()
            except ValueError:
                failures.append(
                    Failure(
                        source_relative,
                        line,
                        "migration directory falls outside its Bazel package",
                    )
                )
                continue
            build_text = (consumer / "BUILD.bazel").read_text(encoding="utf-8")
            data_spec = compile_data_spec(build_text)
            covered = any(
                pattern_covers(pattern, target_name, directory=True)
                for pattern in data_spec.values
            ) and not any(
                pattern_covers(pattern, target_name, directory=True)
                for pattern in data_spec.excludes
            )
            if not covered:
                failures.append(
                    Failure(
                        source_relative,
                        line,
                        f"consumer BUILD.bazel does not cover migration directory {target_name}",
                    )
                )

    for source_relative, expected_count in ALLOWED_DYNAMIC_INCLUDE_COUNTS.items():
        source = root / source_relative
        if (
            source.is_file()
            and observed_dynamic_counts.get(source_relative, 0) != expected_count
        ):
            failures.append(
                Failure(
                    source_relative,
                    1,
                    "dynamic include allowlist is stale: expected "
                    f"{expected_count}, found {observed_dynamic_counts.get(source_relative, 0)}",
                )
            )

    errors = [
        f"{failure.source}:{failure.line}: {failure.message}"
        for failure in sorted(
            failures, key=lambda item: (item.source, item.line, item.message)
        )
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
