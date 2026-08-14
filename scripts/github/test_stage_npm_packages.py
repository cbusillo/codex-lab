#!/usr/bin/env python3
import importlib.util
import sys
import unittest
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "stage_npm_packages.py"


def load_module():
    spec = importlib.util.spec_from_file_location("stage_npm_packages", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load module from {SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


stage_npm_packages = load_module()


class StageNpmPackagesTest(unittest.TestCase):
    def test_expand_packages_expands_root_package_by_default(self) -> None:
        self.assertEqual(
            stage_npm_packages.PACKAGE_EXPANSIONS["codex"],
            stage_npm_packages.expand_packages(["codex"]),
        )

    def test_expand_packages_can_preserve_requested_packages(self) -> None:
        self.assertEqual(
            ["codex", "codex-sdk"],
            stage_npm_packages.expand_packages(
                ["codex", "codex-sdk", "codex"],
                expand=False,
            ),
        )

    def test_parse_args_accepts_no_expand_packages(self) -> None:
        with mock.patch.object(
            sys,
            "argv",
            [
                str(SCRIPT),
                "--release-version",
                "0.0.0",
                "--package",
                "codex",
                "--no-expand-packages",
            ],
        ):
            args = stage_npm_packages.parse_args()

        self.assertTrue(args.no_expand_packages)


if __name__ == "__main__":
    unittest.main()
