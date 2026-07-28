#!/usr/bin/env python3
"""The repo-checks npm staging must not depend on an expiring artifact.

`repo-checks` is a blocking check. It used to stage its npm package from a
pinned `openai/codex` Actions run, which GitHub deletes after 90 days: the check
was scheduled to start failing every PR on a timer, for reasons unrelated to the
PR. These tests pin the replacement contract -- stage the root wrapper from this
checkout, with no network fetch of native artifacts.
"""

import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
REPO_CHECKS = ROOT / ".github/workflows/repo-checks.yml"
STAGE_SCRIPT = ROOT / "scripts/stage_npm_packages.py"

_SPEC = importlib.util.spec_from_file_location("stage_npm_packages", STAGE_SCRIPT)
assert _SPEC is not None and _SPEC.loader is not None
stage_npm_packages = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(stage_npm_packages)


class RepoChecksNpmStagingTest(unittest.TestCase):
    def setUp(self) -> None:
        self.contents = REPO_CHECKS.read_text()

    def test_does_not_reference_a_cross_repo_actions_run(self) -> None:
        self.assertNotIn("actions/runs/", self.contents)

    def test_does_not_pass_a_workflow_url(self) -> None:
        self.assertNotIn("--workflow-url", self.contents)

    def test_stages_without_expanding_platform_packages(self) -> None:
        self.assertIn("--no-expand-packages", self.contents)


class NoExpandPackagesTest(unittest.TestCase):
    """`--no-expand-packages` is what makes the staging artifact-free."""

    def test_expansion_pulls_in_platform_packages(self) -> None:
        expanded = stage_npm_packages.expand_packages(["codex"])

        self.assertEqual(
            expanded,
            ["codex", *stage_npm_packages.CODEX_PLATFORM_PACKAGES],
        )

    def test_no_expansion_stages_only_the_requested_packages(self) -> None:
        self.assertEqual(
            stage_npm_packages.expand_packages(["codex"], expand=False),
            ["codex"],
        )

    def test_no_expansion_deduplicates(self) -> None:
        self.assertEqual(
            stage_npm_packages.expand_packages(["codex", "codex"], expand=False),
            ["codex"],
        )

    def test_the_root_wrapper_needs_no_native_artifacts(self) -> None:
        packages = stage_npm_packages.expand_packages(["codex"], expand=False)

        self.assertEqual(
            stage_npm_packages.collect_native_component_sets(packages),
            [],
        )

    def test_the_expanded_set_does_need_native_artifacts(self) -> None:
        packages = stage_npm_packages.expand_packages(["codex"])

        self.assertNotEqual(
            stage_npm_packages.collect_native_component_sets(packages),
            [],
        )


if __name__ == "__main__":
    unittest.main()
