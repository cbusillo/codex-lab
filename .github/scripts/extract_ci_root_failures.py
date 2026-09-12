#!/usr/bin/env python3

"""Extract bounded, deterministic root failures from GitHub Actions results."""

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


DEFAULT_MAX_INPUT_BYTES = 1_000_000
DEFAULT_MAX_OUTPUT_BYTES = 32_768
DEFAULT_MAX_ROOTS = 50
DEFAULT_MAX_SOURCES_PER_ROOT = 8
MAX_RECORDS = 1_000
MAX_TEXT_LENGTH = 512

AGGREGATOR_JOB_IDS = frozenset({"required", "results", "full verification"})
FAILURE_CONCLUSIONS = frozenset(
    {"action_required", "failure", "startup_failure", "timed_out"}
)
FALLBACK_CONCLUSIONS = frozenset({"cancelled"})
ROOT_FIELDS = (
    "actionable_root",
    "root_failure",
    "root",
    "failure_key",
    "fingerprint",
)


class InputError(ValueError):
    """Raised when the bounded GitHub Actions result input is invalid."""


def _normalise(value: str) -> str:
    return " ".join(re.sub(r"[^a-z0-9]+", " ", value.casefold()).split())


def _bounded_text(record: dict[str, Any], fields: tuple[str, ...], label: str) -> str:
    for field in fields:
        if field not in record or record[field] is None:
            continue
        value = record[field]
        if not isinstance(value, str) or not value.strip():
            raise InputError(f"{label} must be a non-empty string")
        if len(value) > MAX_TEXT_LENGTH:
            raise InputError(f"{label} exceeds the bounded field size")
        return value
    raise InputError(f"missing {label}")


def _identifier(
    record: dict[str, Any], fields: tuple[str, ...], label: str
) -> str | int:
    for field in fields:
        if field not in record or record[field] is None:
            continue
        value = record[field]
        if isinstance(value, bool) or not isinstance(value, (int, str)):
            raise InputError(f"{label} must be a string or integer")
        if isinstance(value, str) and (
            not value.strip() or len(value) > MAX_TEXT_LENGTH
        ):
            raise InputError(f"{label} is empty or exceeds the bounded field size")
        return value
    raise InputError(f"missing {label}")


def _is_aggregator(
    kind: str, identifier: str | int, name: str, record: dict[str, Any]
) -> bool:
    explicit = record.get("is_aggregator")
    if explicit is not None:
        if not isinstance(explicit, bool):
            raise InputError("is_aggregator must be a boolean")
        return explicit

    if kind == "job" and _normalise(str(identifier)) in AGGREGATOR_JOB_IDS:
        return True

    callee = name.rsplit("/", maxsplit=1)[-1]
    normalised_callee = _normalise(callee)
    tokens = set(normalised_callee.split())
    if "aggregator" in tokens or {"fan", "in"}.issubset(tokens):
        return True
    return normalised_callee in {
        "ci required",
        "ci results required",
        "full ci results",
        "full verification",
        "results",
    }


def _conclusion(
    record: dict[str, Any], fields: tuple[str, ...], label: str
) -> str | None:
    for field in fields:
        if field not in record or record[field] in (None, ""):
            continue
        value = record[field]
        if not isinstance(value, str):
            raise InputError(f"{label} must be a string or null")
        if len(value) > MAX_TEXT_LENGTH:
            raise InputError(f"{label} exceeds the bounded field size")
        return value
    return None


def _failed_step_name(record: dict[str, Any]) -> str | None:
    steps = record.get("steps")
    if steps is None:
        return None
    if not isinstance(steps, list) or len(steps) > MAX_RECORDS:
        raise InputError("steps must be a bounded array")
    for step in steps:
        if not isinstance(step, dict):
            raise InputError("each step result must be an object")
        conclusion = _conclusion(step, ("conclusion",), "step conclusion")
        if conclusion and conclusion.strip().casefold() in FAILURE_CONCLUSIONS:
            name = step.get("name")
            if name in (None, ""):
                return None
            if not isinstance(name, str) or len(name) > MAX_TEXT_LENGTH:
                raise InputError("step name must be a bounded string")
            return name
    return None


def _root_name(record: dict[str, Any], name: str) -> str:
    for field in ROOT_FIELDS:
        if field not in record:
            continue
        value = record[field]
        if not isinstance(value, str) or not value.strip():
            raise InputError(f"{field} must be a non-empty string")
        if len(value) > MAX_TEXT_LENGTH:
            raise InputError(f"{field} exceeds the bounded field size")
        return value
    return _failed_step_name(record) or name


def _record_source(
    record: Any,
    *,
    kind: str,
    identifier_fields: tuple[str, ...],
    name_fields: tuple[str, ...],
) -> dict[str, Any]:
    if not isinstance(record, dict):
        raise InputError("each job or check result must be an object")

    identifier = _identifier(record, identifier_fields, f"{kind} identifier")
    name = _bounded_text(record, name_fields, f"{kind} name")
    conclusion = _conclusion(record, ("conclusion", "result"), f"{kind} conclusion")
    root = _root_name(record, name)

    return {
        "kind": kind,
        "id": identifier,
        "name": name,
        "conclusion": conclusion,
        "root": root,
        "root_key": _normalise(root),
        "aggregator": _is_aggregator(kind, identifier, name, record),
    }


def _sources_from_payload(payload: Any) -> list[dict[str, Any]]:
    if not isinstance(payload, dict):
        raise InputError("input must be a JSON object")

    sources: list[dict[str, Any]] = []
    array_specs = (
        ("jobs", "job", ("id", "databaseId", "job_id"), ("name", "job_name")),
        (
            "check_runs",
            "check",
            ("id", "databaseId", "check_run_id", "check_id"),
            ("name", "check_name"),
        ),
        (
            "checkRuns",
            "check",
            ("id", "databaseId", "check_run_id", "check_id"),
            ("name", "check_name"),
        ),
        (
            "checks",
            "check",
            ("id", "databaseId", "check_run_id", "check_id"),
            ("name", "check_name"),
        ),
    )

    present = False
    for field, kind, identifier_fields, name_fields in array_specs:
        if field not in payload:
            continue
        present = True
        records = payload[field]
        if not isinstance(records, list):
            raise InputError(f"{field} must be an array")
        if len(sources) + len(records) > MAX_RECORDS:
            raise InputError("input contains too many job or check results")
        sources.extend(
            _record_source(
                record,
                kind=kind,
                identifier_fields=identifier_fields,
                name_fields=name_fields,
            )
            for record in records
        )

    if not present:
        raise InputError("input must contain jobs or check_runs")
    return sources


def _source_sort_key(source: dict[str, Any]) -> tuple[str, str, str, str]:
    identifier = json.dumps(source["id"], ensure_ascii=False, separators=(",", ":"))
    return (
        source["kind"],
        identifier,
        source["name"],
        str(source["conclusion"]),
    )


def _serialise(report: dict[str, Any]) -> str:
    return json.dumps(report, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _output_limit_error() -> str:
    return _serialise({"error": "output_limit_exceeded"})


def extract_root_failures(
    payload: Any,
    *,
    max_roots: int = DEFAULT_MAX_ROOTS,
    max_sources_per_root: int = DEFAULT_MAX_SOURCES_PER_ROOT,
) -> dict[str, Any]:
    if max_roots < 0 or max_sources_per_root < 1:
        raise ValueError("failure output bounds are invalid")

    groups: dict[str, list[dict[str, Any]]] = {}
    root_names: dict[str, str] = {}
    suppressed_aggregators = 0
    failed_aggregators = 0
    ignored_non_actionable = 0
    sources = _sources_from_payload(payload)
    eligible = []
    for source in sources:
        conclusion = source["conclusion"]
        if source["aggregator"]:
            if conclusion:
                suppressed_aggregators += 1
                if conclusion.strip().casefold() in FAILURE_CONCLUSIONS:
                    failed_aggregators += 1
            continue
        eligible.append(source)
    actionable = [
        source
        for source in eligible
        if source["conclusion"]
        and source["conclusion"].strip().casefold() in FAILURE_CONCLUSIONS
    ]
    if not actionable:
        actionable = [
            source
            for source in eligible
            if source["conclusion"]
            and source["conclusion"].strip().casefold() in FALLBACK_CONCLUSIONS
        ]

    actionable_ids = {id(source) for source in actionable}
    for source in eligible:
        if id(source) not in actionable_ids:
            if (
                source["conclusion"]
                and source["conclusion"].strip().casefold() != "success"
            ):
                ignored_non_actionable += 1
            continue
        root_key = source["root_key"]
        groups.setdefault(root_key, []).append(source)
        root_names[root_key] = min(
            root_names.get(root_key, source["root"]), source["root"]
        )

    ordered_keys = sorted(groups)
    duplicate_count = sum(max(0, len(groups[key]) - 1) for key in ordered_keys)
    truncated = len(ordered_keys) > max_roots or any(
        len(groups[key]) > max_sources_per_root for key in ordered_keys
    )

    failures = []
    for root_key in ordered_keys[:max_roots]:
        grouped_sources = sorted(groups[root_key], key=_source_sort_key)[
            :max_sources_per_root
        ]
        failures.append(
            {
                "root": root_names[root_key],
                "sources": [
                    {
                        "conclusion": source["conclusion"],
                        "id": source["id"],
                        "kind": source["kind"],
                        "name": source["name"],
                    }
                    for source in grouped_sources
                ],
            }
        )

    return {
        "failures": failures,
        "summary": {
            "aggregators_suppressed": suppressed_aggregators,
            "failed_aggregators_suppressed": failed_aggregators,
            "duplicates_collapsed": duplicate_count,
            "non_actionable_ignored": ignored_non_actionable,
            "reported_roots": len(failures),
            "reported_sources": sum(len(failure["sources"]) for failure in failures),
            "total_roots": len(ordered_keys),
            "truncated": truncated,
        },
    }


def bounded_report(report: dict[str, Any], max_output_bytes: int) -> str:
    if max_output_bytes < 1:
        raise ValueError("output byte bound must be positive")

    candidate = json.loads(json.dumps(report))
    candidate["summary"]["truncated"] = bool(candidate["summary"]["truncated"])

    rendered = _serialise(candidate)
    limit_error = _output_limit_error()
    if len(limit_error.encode("utf-8")) > max_output_bytes:
        raise ValueError("output byte bound is too small for a safe error report")
    if len(rendered.encode("utf-8")) <= max_output_bytes:
        return rendered

    candidate["summary"]["truncated"] = True
    for failure in candidate["failures"]:
        failure["sources"] = failure["sources"][:1]
    candidate["summary"]["reported_sources"] = sum(
        len(failure["sources"]) for failure in candidate["failures"]
    )
    rendered = _serialise(candidate)
    while len(rendered.encode("utf-8")) > max_output_bytes and candidate["failures"]:
        candidate["failures"].pop()
        candidate["summary"]["reported_roots"] = len(candidate["failures"])
        candidate["summary"]["reported_sources"] = sum(
            len(failure["sources"]) for failure in candidate["failures"]
        )
        rendered = _serialise(candidate)

    if len(rendered.encode("utf-8")) <= max_output_bytes:
        return rendered
    return limit_error


def _read_input(path: str, max_input_bytes: int) -> Any:
    if max_input_bytes < 1:
        raise ValueError("input byte bound must be positive")
    try:
        if path == "-":
            data = sys.stdin.buffer.read(max_input_bytes + 1)
        else:
            with Path(path).open("rb") as input_file:
                data = input_file.read(max_input_bytes + 1)
    except (OSError, ValueError) as error:
        raise InputError("unable to read input") from error
    if len(data) > max_input_bytes:
        raise InputError("input exceeds the bounded byte size")
    try:
        return json.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise InputError("input is not valid UTF-8 JSON") from error


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", default="-", help="JSON file, or '-' for stdin")
    parser.add_argument("--max-input-bytes", type=int, default=DEFAULT_MAX_INPUT_BYTES)
    parser.add_argument(
        "--max-output-bytes", type=int, default=DEFAULT_MAX_OUTPUT_BYTES
    )
    parser.add_argument("--max-roots", type=int, default=DEFAULT_MAX_ROOTS)
    parser.add_argument(
        "--max-sources-per-root",
        type=int,
        default=DEFAULT_MAX_SOURCES_PER_ROOT,
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        payload = _read_input(args.input, args.max_input_bytes)
        report = extract_root_failures(
            payload,
            max_roots=args.max_roots,
            max_sources_per_root=args.max_sources_per_root,
        )
        print(bounded_report(report, args.max_output_bytes))
    except (InputError, ValueError):
        print("invalid or unbounded CI result input", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
