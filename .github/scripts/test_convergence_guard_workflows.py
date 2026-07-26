"""Lint and contract checks for the workflows and shell this change owns.

Repository-wide `actionlint` and `shellcheck` sweeps are still noisy, so these
tests bound themselves to the convergence-guard and exec-harness surfaces
instead of pretending the whole tree is clean.
"""

import os
import shutil
import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github" / "workflows"

LINTED_WORKFLOWS = (
    WORKFLOWS / "repo-checks.yml",
    WORKFLOWS / "exec-harness.yml",
)
LINTED_SHELL = (ROOT / "scripts" / "local" / "exec-harness-env.sh",)

# CI sets this so a missing linter fails loudly instead of skipping to green.
REQUIRED = os.environ.get("CONVERGENCE_LINT_REQUIRED") == "1"


def require(tool: str, test: unittest.TestCase) -> None:
    if shutil.which(tool) is not None:
        return
    if REQUIRED:
        test.fail(f"{tool} is required when CONVERGENCE_LINT_REQUIRED=1")
    test.skipTest(f"{tool} is not installed")


def run(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=ROOT, capture_output=True, text=True)


class ActionlintTest(unittest.TestCase):
    def setUp(self) -> None:
        require("actionlint", self)

    def test_owned_workflows_pass_actionlint(self) -> None:
        for workflow in LINTED_WORKFLOWS:
            with self.subTest(workflow=workflow.name):
                result = run(["actionlint", str(workflow)])
                self.assertEqual(0, result.returncode, result.stdout or result.stderr)


class ShellcheckTest(unittest.TestCase):
    def setUp(self) -> None:
        require("shellcheck", self)

    def test_owned_shell_scripts_pass_shellcheck(self) -> None:
        for script in LINTED_SHELL:
            with self.subTest(script=script.name):
                result = run(["shellcheck", str(script)])
                self.assertEqual(0, result.returncode, result.stdout or result.stderr)


class RepoCheckWiringTest(unittest.TestCase):
    """The guard is worthless if nothing blocking runs it."""

    def test_repo_checks_runs_the_convergence_guard(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn(
            "python3 .github/scripts/upstream_convergence_guard.py", contents
        )

    def test_repo_checks_runs_the_governance_bootstrap(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn(
            "python3 .github/scripts/verify_upstream_convergence_governance.py",
            contents,
        )

    def test_repo_checks_runs_the_convergence_validator(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn(
            "python3 .github/scripts/upstream_convergence.py validate", contents
        )

    def test_repo_checks_runs_the_guard_and_inventory_tests(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn("test_upstream_convergence_*.py", contents)

    def test_repo_checks_is_reachable_from_blocking_ci(self) -> None:
        contents = (WORKFLOWS / "blocking-ci.yml").read_text(encoding="utf-8")

        self.assertIn("uses: ./.github/workflows/repo-checks.yml", contents)

    def test_repo_checks_runs_the_exec_harness_unit_tests(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn(
            "python3 -m unittest discover -s tools/codex-exec-harness -p 'test_*.py'",
            contents,
        )


if __name__ == "__main__":
    unittest.main()
