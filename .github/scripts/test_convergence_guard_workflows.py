"""Lint and contract checks for the workflows and shell this change owns.

Repository-wide `actionlint` and `shellcheck` sweeps are still noisy, so these
tests bound themselves to the convergence-guard and exec-harness surfaces
instead of pretending the whole tree is clean.
"""

import json
import os
import re
import shutil
import subprocess
import unittest
from pathlib import Path

from verify_repo_checks_test_registration import is_registered


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github" / "workflows"

LINTED_WORKFLOWS = (
    WORKFLOWS / "repo-checks.yml",
    WORKFLOWS / "exec-harness.yml",
    WORKFLOWS / "upstream-convergence.yml",
)
LINTED_SHELL = (
    ROOT / "scripts" / "local" / "cleanup-space.sh",
    ROOT / "scripts" / "local" / "exec-harness-env.sh",
)

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

    def test_repo_checks_runs_the_contract_gate_verifier(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn(
            "python3 .github/scripts/upstream_convergence_gates.py", contents
        )

    def test_repo_checks_runs_the_bazel_data_edge_verifier(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn(
            "python3 .github/scripts/upstream_convergence_bazel_data.py", contents
        )

    def test_repo_checks_runs_the_convergence_validator(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn(
            "python3 .github/scripts/upstream_convergence.py validate", contents
        )
        self.assertIn("fetch-depth: 0", contents)
        self.assertIn("git remote add openai https://github.com/openai/codex.git", contents)
        self.assertIn('--against "$CONVERGENCE_BASE_SHA"', contents)
        self.assertIn('--json | tee "$report"', contents)
        self.assertIn("Convergence comparison base:", contents)
        self.assertIn("$GITHUB_STEP_SUMMARY", contents)

    def test_bazel_does_not_run_history_dependent_github_script_suite(self) -> None:
        contents = (WORKFLOWS / "bazel.yml").read_text(encoding="utf-8")

        self.assertNotIn("just test-github-scripts", contents)

    def test_convergence_summary_jq_program_executes(self) -> None:
        require("jq", self)
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")
        match = re.search(
            r"jq -r '\n(?P<program>.*?)\n\s*' \"\$report\"",
            contents,
            flags=re.DOTALL,
        )
        self.assertIsNotNone(match)
        program = match.group("program")
        result = subprocess.run(
            ["jq", "-r", program],
            cwd=ROOT,
            input=json.dumps(
                {
                    "comparisonMode": "bootstrap",
                    "policyStateAtBase": "absent",
                    "appendOnlyChecked": False,
                    "provenanceChecked": False,
                    "bootstrapReason": None,
                    "newSnapshots": ["one", "two"],
                }
            ),
            capture_output=True,
            text=True,
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("Comparison mode: `bootstrap`", result.stdout)
        self.assertIn("Bootstrap reason: none", result.stdout)
        self.assertIn("New snapshots: `one, two`", result.stdout)

    def test_repo_checks_runs_the_guard_and_inventory_tests(self) -> None:
        # Asserted through the registration verifier rather than a literal
        # pattern string: `repo-checks.yml` discovers the whole directory, so
        # pinning one spelling of the pattern would break on every valid change
        # to how discovery is expressed.
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        for name in (
            "test_upstream_convergence_guard.py",
            "test_upstream_convergence_inventory.py",
        ):
            with self.subTest(name=name):
                self.assertTrue(is_registered(name, ".github/scripts", contents))

    def test_repo_checks_is_reachable_from_blocking_ci(self) -> None:
        contents = (WORKFLOWS / "blocking-ci.yml").read_text(encoding="utf-8")

        self.assertIn("uses: ./.github/workflows/repo-checks.yml", contents)

    def test_repo_checks_runs_the_exec_harness_unit_tests(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn(
            "python3 -m unittest discover -s tools/codex-exec-harness -p 'test_*.py'",
            contents,
        )


class UpstreamConvergenceWorkflowTest(unittest.TestCase):
    def setUp(self) -> None:
        self.contents = (WORKFLOWS / "upstream-convergence.yml").read_text(
            encoding="utf-8"
        )

    def step(self, name: str) -> str:
        match = re.search(
            rf"^      - name: {re.escape(name)}\n(?P<body>.*?)(?=^      - name: |\Z)",
            self.contents,
            flags=re.DOTALL | re.MULTILINE,
        )
        self.assertIsNotNone(match)
        return match.group("body")

    def test_reports_are_written_outside_the_checkout(self) -> None:
        self.assertIn('report_dir="$RUNNER_TEMP/upstream-convergence"', self.contents)
        self.assertIn('--json > "$raw_report"', self.contents)
        self.assertIn(
            '--output "$RUNNER_TEMP/upstream-convergence/convergence-summary.json"',
            self.contents,
        )
        self.assertNotIn("> report.json", self.contents)
        self.assertNotIn("path: convergence-summary.json", self.contents)

    def test_inspection_evidence_is_preserved_before_failure(self) -> None:
        self.assertIn('PYTHONDONTWRITEBYTECODE: "1"', self.contents)
        inspection = self.step("Run convergence inspection")
        diagnostics = self.step("Print bounded inspection diagnostics")
        clean_check = self.step("Verify inspection leaves checkout clean")
        raw_upload = self.step("Upload raw inspection report")
        failure = self.step("Fail when inspection fails")

        self.assertIn("id: inspection", inspection)
        self.assertIn("set -euo pipefail", inspection)
        self.assertIn("if not isinstance(report, dict):", inspection)
        self.assertIn('echo "status=$status"', inspection)
        self.assertIn('echo "started_at=$started_at"', inspection)
        self.assertIn('echo "finished_at=$finished_at"', inspection)
        self.assertIn('echo "duration_ms=$duration_ms"', inspection)
        self.assertIn('} >> "$GITHUB_OUTPUT"', inspection)
        self.assertIn('INSPECTION_STARTED_AT="$started_at"', inspection)
        self.assertIn('INSPECTION_DURATION_MS="$duration_ms"', inspection)
        self.assertIn('"inspectionStartedAt": os.environ["INSPECTION_STARTED_AT"]', inspection)
        self.assertIn('"inspectionDurationMs": int(os.environ["INSPECTION_DURATION_MS"])', inspection)
        for step in (diagnostics, clean_check, raw_upload, failure):
            self.assertIn("always() && !cancelled()", step)
            self.assertIn("steps.inspection.outcome != 'skipped'", step)
        self.assertIn('test -f "$RUNNER_TEMP/upstream-convergence/report.json"', diagnostics)
        self.assertIn('cat "$RUNNER_TEMP/upstream-convergence/diagnostics.json"', diagnostics)
        self.assertIn("if-no-files-found: error", raw_upload)
        self.assertIn(
            "steps.inspection.outputs.status != '0'", failure
        )
        self.assertNotIn("continue-on-error: true", self.contents)

    def test_compact_report_only_runs_after_success(self) -> None:
        publish = self.step("Publish compact report")
        upload = self.step("Upload compact report")

        for step in (self.step("Find newest successful report"), publish, upload):
            self.assertIn(
                "success() && steps.inspection.outputs.status == '0'", step
            )
        self.assertIn(
            'PREVIOUS_SUCCESS_AT: ${{ steps.previous.outputs.updated_at }}', publish
        )
        self.assertIn(
            'INSPECTION_STARTED_AT: ${{ steps.inspection.outputs.started_at }}',
            publish,
        )
        self.assertIn(
            'INSPECTION_FINISHED_AT: ${{ steps.inspection.outputs.finished_at }}',
            publish,
        )
        self.assertIn(
            'INSPECTION_DURATION_MS: ${{ steps.inspection.outputs.duration_ms }}',
            publish,
        )
        self.assertIn('--previous-success-at "$PREVIOUS_SUCCESS_AT"', publish)
        self.assertIn("name: upstream-convergence-${{ github.run_id }}", upload)
        self.assertIn(
            "path: ${{ runner.temp }}/upstream-convergence/convergence-summary.json",
            upload,
        )
        self.assertIn("retention-days: 90", upload)

    def test_permissions_remain_read_only(self) -> None:
        self.assertIn("permissions:\n  actions: read\n  contents: read", self.contents)

    def test_workflow_has_no_model_or_agent_steps(self) -> None:
        self.assertNotRegex(self.contents, r"\b(?:code|codex)\s+exec\b")
        self.assertNotRegex(self.contents, r"\b(?:claude|gemini)\b")
        self.assertNotIn("gh issue create", self.contents)
        self.assertNotIn("gh pr create", self.contents)
        self.assertNotIn("git push", self.contents)
        self.assertNotIn("create-pull-request", self.contents)
        self.assertNotIn("contents: write", self.contents)


if __name__ == "__main__":
    unittest.main()
