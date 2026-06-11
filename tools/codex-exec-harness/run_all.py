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
DEFAULT_CODEX_BIN = ROOT / "codex-rs" / "target" / "debug" / "codex"
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
    return {field: left.get(field, 0) + right.get(field, 0) for field in TOKEN_USAGE_FIELDS}


def load_summary(stdout: str) -> dict[str, object] | None:
    try:
        value = json.loads(stdout)
    except json.JSONDecodeError:
        return None
    return value if isinstance(value, dict) else None


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


def save_report(path: Path, report: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(report, handle, indent=2, sort_keys=True)
        print(file=handle)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    scenarios = sorted(SCENARIO_DIR.glob("*.json"))
    if not scenarios:
        print(f"no scenarios found in {SCENARIO_DIR}", file=sys.stderr)
        return 2

    started_at = time.time()
    scenario_results: list[dict[str, object]] = []
    aggregate_tokens = empty_token_usage()
    exit_code = 0

    for scenario in scenarios:
        print(f"== {scenario.relative_to(ROOT)}", flush=True)
        command = [
            sys.executable,
            str(HARNESS),
            str(scenario),
            "--codex-bin",
            args.codex_bin,
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
                {key: value for key, value in token_usage.items() if isinstance(value, int)},
            )
        scenario_results.append(summary)
        if result.returncode != 0:
            exit_code = result.returncode
            break

    report = {
        "schema_version": 1,
        "codex_bin": args.codex_bin,
        "git_revision": git_revision(),
        "output_root": args.output_root,
        "passed": exit_code == 0,
        "partial": exit_code != 0 and len(scenario_results) < len(scenarios),
        "returncode": exit_code,
        "scenario_count": len(scenario_results),
        "scenario_total": len(scenarios),
        "wall_seconds": round(time.time() - started_at, 3),
        "token_usage": aggregate_tokens,
        "scenarios": scenario_results,
    }
    if args.report_json is not None:
        save_report(Path(args.report_json), report)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
