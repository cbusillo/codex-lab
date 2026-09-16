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

        self.assertIn("python3 .github/scripts/upstream_convergence_guard.py", contents)

    def test_repo_checks_runs_the_governance_bootstrap(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn(
            "python3 .github/scripts/verify_upstream_convergence_governance.py",
            contents,
        )

    def test_repo_checks_runs_the_contract_gate_verifier(self) -> None:
        contents = (WORKFLOWS / "repo-checks.yml").read_text(encoding="utf-8")

        self.assertIn("python3 .github/scripts/upstream_convergence_gates.py", contents)

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
        self.assertIn(
            "git remote add openai https://github.com/openai/codex.git", contents
        )
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
        self.assertIn(
            '"inspectionStartedAt": os.environ["INSPECTION_STARTED_AT"]', inspection
        )
        self.assertIn(
            '"inspectionDurationMs": int(os.environ["INSPECTION_DURATION_MS"])',
            inspection,
        )
        for step in (diagnostics, clean_check, raw_upload, failure):
            self.assertIn("always() && !cancelled()", step)
            self.assertIn("steps.inspection.outcome != 'skipped'", step)
        self.assertIn(
            'test -f "$RUNNER_TEMP/upstream-convergence/report.json"', diagnostics
        )
        self.assertIn(
            'cat "$RUNNER_TEMP/upstream-convergence/diagnostics.json"', diagnostics
        )
        self.assertIn("if-no-files-found: error", raw_upload)
        self.assertIn("steps.inspection.outputs.status != '0'", failure)
        self.assertNotIn("continue-on-error: true", self.contents)

    def test_compact_report_only_runs_after_success(self) -> None:
        publish = self.step("Publish compact report")
        upload = self.step("Upload compact report")

        for step in (self.step("Find newest successful report"), publish, upload):
            self.assertIn("success() && steps.inspection.outputs.status == '0'", step)
        self.assertIn(
            "PREVIOUS_SUCCESS_AT: ${{ steps.previous.outputs.updated_at }}", publish
        )
        self.assertIn(
            "INSPECTION_STARTED_AT: ${{ steps.inspection.outputs.started_at }}",
            publish,
        )
        self.assertIn(
            "INSPECTION_FINISHED_AT: ${{ steps.inspection.outputs.finished_at }}",
            publish,
        )
        self.assertIn(
            "INSPECTION_DURATION_MS: ${{ steps.inspection.outputs.duration_ms }}",
            publish,
        )
        self.assertIn('--previous-success-at "$PREVIOUS_SUCCESS_AT"', publish)
        self.assertIn("name: upstream-convergence-${{ github.run_id }}", upload)
        self.assertIn(
            "path: ${{ runner.temp }}/upstream-convergence/convergence-summary.json",
            upload,
        )
        self.assertIn("retention-days: 90", upload)

    def test_inspect_exports_bounded_candidate_inputs(self) -> None:
        inspect_job = re.search(
            r"^  inspect:\n(?P<body>.*?)(?=^  candidate:|\Z)",
            self.contents,
            flags=re.DOTALL | re.MULTILINE,
        )
        self.assertIsNotNone(inspect_job)
        inspect_contents = inspect_job.group("body")
        publish = self.step("Publish compact report")

        for output in (
            "candidate_due",
            "candidate_gate_status",
            "candidate_base",
            "candidate_upstream",
            "candidate_local",
            "candidate_snapshot",
        ):
            self.assertIn(
                f"{output}: ${{{{ steps.summary.outputs.{output} }}}}", inspect_contents
            )
            self.assertIn(f"{output}=", publish)
        self.assertIn("id: summary", publish)
        self.assertIn("exact_ref = re.compile", publish)
        self.assertIn("[0-9a-f]{40}", publish)
        self.assertIn('snapshot["snapshot"][:512]', publish)
        self.assertIn('"\\n" in status', publish)
        self.assertIn('"\\n" in snapshot_path', publish)

    def test_candidate_job_is_read_only_and_independently_serialized(self) -> None:
        candidate = re.search(
            r"^  candidate:\n(?P<body>.*)\Z",
            self.contents,
            flags=re.DOTALL | re.MULTILINE,
        )
        self.assertIsNotNone(candidate)
        contents = candidate.group("body")

        self.assertIn("needs: inspect", contents)
        self.assertIn(
            "needs.inspect.outputs.candidate_due == 'true' && needs.inspect.outputs.candidate_gate_status == 'ready' && github.ref == 'refs/heads/main'",
            contents,
        )
        self.assertIn("runs-on: macos-26", contents)
        self.assertIn("permissions:\n      contents: read", contents)
        self.assertNotIn("actions: read", contents)
        self.assertIn(
            "group: upstream-convergence-candidate-${{ github.ref }}", contents
        )
        self.assertIn("persist-credentials: false", contents)
        self.assertNotIn("GH_TOKEN", contents)
        self.assertNotIn("secrets.", contents)

    def test_candidate_stage_uses_trusted_data_only_preflight_and_cleanup(self) -> None:
        candidate = self.step(
            "Re-fetch canonical refs and run data-only candidate preflight"
        )
        verify = self.step("Verify candidate cleanup and primary checkout")
        upload = self.step("Upload candidate evidence")

        self.assertIn("https://github.com/cbusillo/codex-lab.git", candidate)
        self.assertIn("https://github.com/openai/codex.git", candidate)
        self.assertIn(
            "git fetch --no-tags origin +refs/heads/main:refs/remotes/origin/main",
            candidate,
        )
        self.assertIn(
            "git fetch --no-tags openai +refs/heads/main:refs/remotes/openai/main",
            candidate,
        )
        self.assertIn(
            'cp .github/scripts/upstream_candidate_preflight.py "$trusted_helper"',
            candidate,
        )
        self.assertIn(
            "git remote set-url origin https://github.com/cbusillo/codex-lab.git",
            candidate,
        )
        self.assertIn(
            'worktree add --detach "$candidate_dir" "$EXPECTED_UPSTREAM"', candidate
        )
        self.assertIn(
            'core.hooksPath=/dev/null merge --no-commit --no-ff "$EXPECTED_LOCAL"',
            candidate,
        )
        self.assertIn(
            'all_conflicts="$RUNNER_TEMP/upstream-convergence-all-conflicts.txt"',
            candidate,
        )
        self.assertNotIn("$evidence_dir/all-conflict-paths.txt", candidate)
        self.assertIn("sed -n '1,200p'", candidate)
        self.assertIn('python3 "$trusted_helper" preflight', candidate)
        self.assertNotIn("python3 .github/scripts/", candidate)
        self.assertNotIn("GITHUB_WORKSPACE", candidate)
        self.assertNotIn("rusty_v8_bazel.py", candidate)
        self.assertNotIn("cargo check", candidate)
        self.assertNotIn("just test", candidate)
        self.assertIn("cleanup_worktree", candidate)
        self.assertIn("git worktree remove --force", candidate)
        self.assertIn("git status --porcelain=v1 --untracked-files=all", candidate)
        self.assertIn("always() && !cancelled()", verify)
        self.assertIn('[[ -e "$RUNNER_TEMP/upstream-convergence-candidate" ]]', verify)
        self.assertIn("worktree_removed=false", verify)
        self.assertIn("retention-days: 90", upload)
        self.assertLess(
            candidate.index("git fetch --no-tags origin"),
            candidate.index('worktree add --detach "$candidate_dir"'),
        )
        self.assertLess(
            candidate.index('worktree add --detach "$candidate_dir"'),
            candidate.index("core.hooksPath=/dev/null merge --no-commit --no-ff"),
        )
        self.assertLess(
            candidate.index("core.hooksPath=/dev/null merge --no-commit --no-ff"),
            candidate.index('python3 "$trusted_helper" preflight'),
        )

    def test_candidate_stage_has_no_write_or_model_automation(self) -> None:
        candidates = [
            self.step("Re-fetch canonical refs and run data-only candidate preflight"),
            self.step("Run bounded candidate checks and select affected contracts"),
        ]

        for candidate in candidates:
            for forbidden in (
                "git push",
                "git commit",
                "gh pr",
                "gh issue",
                "create-pull-request",
                "code exec",
                "codex exec",
                "claude",
                "gemini",
                "npm ",
                "just test",
            ):
                self.assertNotIn(forbidden, candidate)
        self.assertNotIn("cargo ", candidates[0])

    def test_candidate_stage3b_is_clean_path_only_and_ordered(self) -> None:
        preflight = self.step(
            "Re-fetch canonical refs and run data-only candidate preflight"
        )
        toolchain = self.step("Install pinned Rust toolchain")
        checks = self.step("Run bounded candidate checks and select affected contracts")

        self.assertIn("classification=clean", preflight)
        self.assertIn('echo "stage3b_ready=true"', preflight)
        self.assertIn("stage3b_ready == 'true'", toolchain)
        self.assertIn(
            "dtolnay/rust-toolchain@e081816240890017053eacbb1bdf337761dc5582", toolchain
        )
        self.assertIn("toolchain: 1.95.0", toolchain)
        self.assertIn(
            "python3 .github/scripts/upstream_convergence_gates.py --json", checks
        )
        self.assertIn(
            "python3 .github/scripts/upstream_convergence_guard.py --json", checks
        )
        self.assertIn(
            "python3 .github/scripts/verify_upstream_convergence_governance.py --json",
            checks,
        )
        self.assertIn(
            "python3 .github/scripts/verify_repo_checks_test_registration.py", checks
        )
        self.assertIn(
            '(cd "$candidate_dir" && sandbox-exec -f "$sandbox_profile"', checks
        )
        self.assertIn("cargo check --workspace --tests --locked", checks)
        self.assertEqual(checks.count("cargo check --workspace --tests --locked"), 3)
        self.assertLess(
            checks.index("verify_repo_checks_test_registration.py"),
            checks.index("cargo check --workspace --tests --locked"),
        )
        self.assertIn('CARGO_INCREMENTAL: "0"', checks)
        self.assertIn('RUSTFLAGS: "-C debuginfo=0"', checks)
        self.assertIn(
            "CARGO_TARGET_DIR: ${{ runner.temp }}/upstream-convergence-cargo-target",
            checks,
        )
        self.assertIn(
            "RUSTY_V8_ARCHIVE: ${{ runner.temp }}/upstream-convergence-candidate-downloads/",
            checks,
        )
        self.assertIn(
            "RUSTY_V8_SRC_BINDING_PATH: ${{ runner.temp }}/upstream-convergence-candidate-downloads/",
            checks,
        )
        self.assertLess(
            checks.index("cargo check --workspace --tests --locked"),
            checks.index("select-affected-contracts"),
        )
        self.assertIn("sandbox-exec -f", checks)
        self.assertIn("verify_trusted", checks)

    def test_candidate_stage3b_bounds_evidence_and_uses_trusted_extractors(
        self,
    ) -> None:
        checks = self.step("Run bounded candidate checks and select affected contracts")
        helper = self.step(
            "Re-fetch canonical refs and run data-only candidate preflight"
        )

        self.assertIn("bound-log", checks)
        self.assertIn("--max-input-bytes 65536", checks)
        self.assertIn("--max-output-bytes 32768", checks)
        self.assertIn("--max-roots 50", checks)
        self.assertIn("--max-sources-per-root 8", checks)
        self.assertIn("extract_ci_root_failures.py", helper)
        self.assertIn("select-affected-contracts", checks)
        self.assertIn("upstream-changed-paths.txt", checks)
        self.assertIn("local-changed-paths.txt", checks)
        self.assertIn("record-stage3b", checks)
        self.assertIn("repo-check-outcome.json", checks)
        self.assertIn("cargo-check-outcome.json", checks)
        self.assertIn("affected-contracts.json", checks)
        self.assertIn("root-failure-outcome.json", checks)
        self.assertIn("::warning title=Candidate repository-check regression::", checks)
        self.assertIn("::warning title=Candidate cargo regression::", checks)
        verify = self.step("Verify candidate cleanup and primary checkout")
        self.assertIn('evidence["temporaryWorktreeRemoved"]', verify)
        self.assertIn('evidence["primaryCheckoutClean"]', verify)

    def test_stage3c_runs_on_live_conflicts_and_plumbs_guard_digest(self) -> None:
        helper = self.step(
            "Re-fetch canonical refs and run data-only candidate preflight"
        )
        checks = self.step("Run bounded candidate checks and select affected contracts")
        packets = self.step("Build bounded model packets")

        self.assertIn('cp upstream/convergence-guard.json "$trusted_guard"', helper)
        self.assertIn("trusted_guard_sha", helper)
        self.assertIn("TRUSTED_GUARD_SHA", checks)
        self.assertIn('trusted_guard"', checks)
        self.assertIn("TRUSTED_GUARD_SHA", packets)
        self.assertIn("build-packets", packets)
        self.assertIn("conflict-paths.txt", packets)
        self.assertIn("root-failure-outcome.json", packets)
        self.assertIn("always() && !cancelled()", packets)
        self.assertNotIn("stage3b_ready", packets)
        self.assertLess(
            self.contents.index("Build bounded model packets"),
            self.contents.index("Verify candidate cleanup"),
        )

    def test_stage3d_is_trusted_ordered_and_live_read_only(self) -> None:
        helper = self.step(
            "Re-fetch canonical refs and run data-only candidate preflight"
        )
        packets = self.step("Build bounded model packets")
        checkpoint = self.step("Checkpoint repair cycles and build handoff")
        upload = self.step("Upload candidate evidence")
        self.assertIn(
            'cp .github/scripts/upstream_convergence_repair_ledger.py "$trusted_repair_ledger"',
            helper,
        )
        self.assertIn("trusted_repair_ledger_sha", helper)
        self.assertIn("always() && !cancelled()", checkpoint)
        self.assertNotIn("stage3b_ready", checkpoint)
        self.assertIn("--require-live", checkpoint)
        self.assertIn("RUNNER_TEMP/upstream-convergence-repair-ledger.json", checkpoint)
        self.assertIn("model-packets.json", checkpoint)
        self.assertIn("model-telemetry.json", checkpoint)
        self.assertIn("repair-handoff.txt", checkpoint)
        self.assertIn("TRUSTED_REPAIR_LEDGER_SHA", checkpoint)
        self.assertNotIn("secrets.", checkpoint)
        self.assertNotIn("GH_TOKEN", checkpoint)
        self.assertNotIn("git push", checkpoint)
        self.assertNotIn("gh ", checkpoint)
        self.assertLess(
            self.contents.index("Checkpoint repair cycles and build handoff"),
            self.contents.index("Verify candidate cleanup and primary checkout"),
        )
        self.assertIn("retention-days: 90", upload)

    def test_stage3d_artifacts_are_bounded_and_evidence_is_preserved(self) -> None:
        checkpoint = self.step("Checkpoint repair cycles and build handoff")
        self.assertIn('test -f "$evidence_dir/repair-ledger.json"', checkpoint)
        self.assertIn('test -f "$evidence_dir/repair-checkpoint.json"', checkpoint)
        self.assertIn('wc -c < "$evidence_dir/repair-handoff.txt"', checkpoint)
        self.assertIn("<= 8192", checkpoint)
        self.assertLess(
            self.contents.index("Checkpoint repair cycles and build handoff"),
            self.contents.index("Upload candidate evidence"),
        )
        self.assertIn(
            "${{ runner.temp }}/upstream-convergence-candidate-evidence",
            self.step("Upload candidate evidence"),
        )

    def test_stage3c_preserves_model_free_read_only_controls(self) -> None:
        packets = self.step("Build bounded model packets")

        for forbidden in (
            "git push",
            "git commit",
            "gh pr",
            "gh issue",
            "code exec",
            "codex exec",
            "just test",
            "cargo ",
        ):
            self.assertNotIn(forbidden, packets)
        self.assertIn("model-packets.json", packets)
        self.assertIn("model-packets.txt", packets)
        self.assertIn("model-telemetry.json", packets)
        self.assertIn("test -f", packets)

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
