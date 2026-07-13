#!/usr/bin/env python3
"""Run all Codex exec harness scenarios."""

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCENARIO_DIR = ROOT / "tools" / "codex-exec-harness" / "scenarios"
HARNESS = ROOT / "tools" / "codex-exec-harness" / "harness.py"
PROVENANCE_HELPER = ROOT / "scripts" / "local" / "codex_lab_provenance.py"
DEFAULT_CODEX_BIN = ROOT / "codex-rs" / "target" / "debug" / "codex"
PROVENANCE_TIMEOUT_SECONDS = 120
TOKEN_USAGE_FIELDS = [
    "input_tokens",
    "cached_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
]


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--codex-bin",
        default=str(DEFAULT_CODEX_BIN),
        help="Path to the codex binary under test",
    )
    parser.add_argument(
        "--output-root",
        default=None,
        help="Directory for isolated runs and artifacts",
    )
    parser.add_argument(
        "--report-json",
        default=None,
        help="Write an aggregate eval report JSON to this path",
    )
    return parser.parse_args(argv)


def empty_token_usage() -> dict[str, int]:
    return {field: 0 for field in TOKEN_USAGE_FIELDS}


def add_token_usage(left: dict[str, int], right: dict[str, int]) -> dict[str, int]:
    return {
        field: left.get(field, 0) + right.get(field, 0) for field in TOKEN_USAGE_FIELDS
    }


def load_summary(stdout: str) -> dict[str, object] | None:
    try:
        value = json.loads(stdout)
    except json.JSONDecodeError:
        return None
    return value if isinstance(value, dict) else None


def load_scenario(path: Path) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {}
    return value if isinstance(value, dict) else {}


def git_revision() -> str | None:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if result.returncode != 0:
        return None
    return result.stdout.strip()


def unavailable_provenance(binary_path: str, failure: str) -> dict[str, object]:
    try:
        display_path = str(Path(binary_path).expanduser().resolve(strict=False))
    except (OSError, RuntimeError, ValueError):
        display_path = binary_path
    return {
        "schema_version": 1,
        "status": "unverifiable",
        "binary_path": display_path,
        "expected": None,
        "provenance": None,
        "failures": [failure],
    }


def verify_provenance(binary_path: str) -> dict[str, object]:
    try:
        requested_binary = Path(binary_path).expanduser()
        if not requested_binary.is_absolute():
            requested_binary = Path.cwd() / requested_binary
        binary_path = str(requested_binary.resolve(strict=False))
    except (OSError, RuntimeError, ValueError) as error:
        return unavailable_provenance(binary_path, f"invalid binary path: {error}")
    command = [
        sys.executable,
        str(PROVENANCE_HELPER),
        "--repo-root",
        str(ROOT),
        "--binary",
        binary_path,
        "--verify-only",
        "--json",
    ]
    try:
        result = subprocess.run(
            command,
            cwd=ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=PROVENANCE_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired:
        return unavailable_provenance(binary_path, "provenance verification timed out")
    except OSError as error:
        return unavailable_provenance(
            binary_path, f"could not run provenance verification: {error}"
        )

    report = load_summary(result.stdout)
    if report is None:
        detail = result.stderr.strip() or "helper did not emit a JSON report"
        return unavailable_provenance(binary_path, detail)
    status = report.get("status")
    if status not in {"current", "stale", "unverifiable"}:
        return unavailable_provenance(
            binary_path, "helper emitted an unsupported provenance status"
        )
    if (status == "current") != (result.returncode == 0):
        return unavailable_provenance(
            binary_path, "helper returned an inconsistent provenance result"
        )
    return report


def provenance_revision(report: dict[str, object]) -> str | None:
    expected = report.get("expected")
    if isinstance(expected, dict):
        source_commit = expected.get("source_commit")
        if isinstance(source_commit, str):
            return source_commit
    return git_revision()


def print_provenance_failure(report: dict[str, object]) -> None:
    print(f"Codex Lab provenance {report['status']}", file=sys.stderr)
    failures = report.get("failures")
    if isinstance(failures, list):
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)


def save_report(path: Path, report: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(report, handle, indent=2, sort_keys=True)
        print(file=handle)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    scenarios = [
        scenario
        for scenario in sorted(SCENARIO_DIR.glob("*.json"))
        if load_scenario(scenario).get("skip_run_all") is not True
    ]
    if not scenarios:
        print(f"no scenarios found in {SCENARIO_DIR}", file=sys.stderr)
        return 2

    started_at = time.time()
    scenario_results: list[dict[str, object]] = []
    aggregate_tokens = empty_token_usage()
    exit_code = 0
    provenance = verify_provenance(args.codex_bin)
    verified_binary = provenance.get("binary_path")
    if not isinstance(verified_binary, str):
        verified_binary = args.codex_bin

    if provenance["status"] != "current":
        print_provenance_failure(provenance)
        report = {
            "schema_version": 1,
            "codex_bin": verified_binary,
            "git_revision": provenance_revision(provenance),
            "output_root": args.output_root,
            "passed": False,
            "partial": False,
            "returncode": 1,
            "scenario_count": 0,
            "scenario_total": len(scenarios),
            "wall_seconds": round(time.time() - started_at, 3),
            "token_usage": aggregate_tokens,
            "provenance": provenance,
            "scenarios": scenario_results,
        }
        if args.report_json is not None:
            save_report(Path(args.report_json), report)
        return 1

    for scenario in scenarios:
        print(f"== {scenario.relative_to(ROOT)}", flush=True)
        command = [
            sys.executable,
            str(HARNESS),
            str(scenario),
            "--codex-bin",
            verified_binary,
        ]
        if args.output_root is not None:
            command.extend(["--output-root", args.output_root])
        scenario_started_at = time.time()
        result = subprocess.run(
            command,
            cwd=ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if result.stdout:
            print(result.stdout, end="")
        if result.stderr:
            print(result.stderr, end="", file=sys.stderr)

        summary = load_summary(result.stdout)
        if summary is None:
            summary = {
                "scenario": scenario.stem,
                "scenario_path": str(scenario),
                "returncode": result.returncode,
                "passed": False,
                "failures": ["harness did not emit a JSON summary"],
            }
        summary["wall_seconds"] = round(time.time() - scenario_started_at, 3)
        summary["harness_returncode"] = result.returncode
        token_usage = summary.get("token_usage")
        if isinstance(token_usage, dict):
            aggregate_tokens = add_token_usage(
                aggregate_tokens,
                {
                    key: value
                    for key, value in token_usage.items()
                    if isinstance(value, int)
                },
            )
        scenario_results.append(summary)
        if result.returncode != 0:
            exit_code = result.returncode
            break

    report = {
        "schema_version": 1,
        "codex_bin": verified_binary,
        "git_revision": provenance_revision(provenance),
        "output_root": args.output_root,
        "passed": exit_code == 0,
        "partial": exit_code != 0 and len(scenario_results) < len(scenarios),
        "returncode": exit_code,
        "scenario_count": len(scenario_results),
        "scenario_total": len(scenarios),
        "wall_seconds": round(time.time() - started_at, 3),
        "token_usage": aggregate_tokens,
        "provenance": provenance,
        "scenarios": scenario_results,
    }
    if args.report_json is not None:
        save_report(Path(args.report_json), report)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
