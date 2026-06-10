#!/usr/bin/env python3
"""Run all Codex exec harness scenarios."""

import argparse
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCENARIO_DIR = ROOT / "tools" / "codex-exec-harness" / "scenarios"
HARNESS = ROOT / "tools" / "codex-exec-harness" / "harness.py"
DEFAULT_CODEX_BIN = ROOT / "codex-rs" / "target" / "debug" / "codex"


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--codex-bin",
        default=str(DEFAULT_CODEX_BIN),
        help="Path to the codex binary under test",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    scenarios = sorted(SCENARIO_DIR.glob("*.json"))
    if not scenarios:
        print(f"no scenarios found in {SCENARIO_DIR}", file=sys.stderr)
        return 2

    for scenario in scenarios:
        print(f"== {scenario.relative_to(ROOT)}", flush=True)
        result = subprocess.run(
            [sys.executable, str(HARNESS), str(scenario), "--codex-bin", args.codex_bin],
            cwd=ROOT,
        )
        if result.returncode != 0:
            return result.returncode
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
