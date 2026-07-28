import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
FULL_CI_WORKFLOW = ROOT / ".github/workflows/full-ci.yml"
LEGACY_POSTMERGE_WORKFLOW = ROOT / ".github/workflows/postmerge-ci.yml"
BLOCKING_CI_WORKFLOW = ROOT / ".github/workflows/blocking-ci.yml"
RUST_FULL_CI_WORKFLOW = ROOT / ".github/workflows/rust-ci-full.yml"
V8_CANARY_WORKFLOW = ROOT / ".github/workflows/v8-canary.yml"
CODEX_LAB_RELEASE_WORKFLOW = ROOT / ".github/workflows/codex-lab-release.yml"
FULL_CI_COMPONENTS = (
    ROOT / ".bazelrc",
    ROOT / ".github/workflows/sdk-integration.yml",
    ROOT / ".github/workflows/bazel.yml",
    ROOT / ".github/workflows/rust-ci-full.yml",
    ROOT / ".github/workflows/rust-ci-full-nextest-platform.yml",
    ROOT / ".github/workflows/v8-canary.yml",
    ROOT / ".github/scripts/run-argument-comment-lint-bazel.sh",
)
FULL_VERIFICATION_WORKFLOWS = {
    "bazel.yml",
    "rust-ci-full.yml",
    "sdk-integration.yml",
    "v8-canary.yml",
}
LOCAL_WORKFLOW_CALL = re.compile(
    r"uses:\s+\./\.github/workflows/([A-Za-z0-9_.-]+\.ya?ml)"
)


def called_workflows(workflow_path: Path) -> set[str]:
    return set(LOCAL_WORKFLOW_CALL.findall(workflow_path.read_text()))


def reusable_workflow_depth(workflow_path: Path, stack: tuple[Path, ...] = ()) -> int:
    if workflow_path in stack:
        cycle = " -> ".join(path.name for path in (*stack, workflow_path))
        raise AssertionError(f"reusable workflow cycle: {cycle}")
    callees = called_workflows(workflow_path)
    if not callees:
        return 1
    return 1 + max(
        reusable_workflow_depth(
            workflow_path.parent / callee,
            (*stack, workflow_path),
        )
        for callee in callees
    )


class FullCiTriggerPolicyTest(unittest.TestCase):
    def setUp(self) -> None:
        workflow = FULL_CI_WORKFLOW.read_text()
        self.workflow_header = workflow.split("\njobs:\n", maxsplit=1)[0]

    def test_full_ci_is_not_triggered_by_repository_changes(self) -> None:
        self.assertNotIn("\n  push:", self.workflow_header)
        self.assertNotIn("\n  pull_request:", self.workflow_header)

    def test_full_ci_is_scheduled_and_manually_dispatchable(self) -> None:
        self.assertIn("\n  workflow_dispatch:", self.workflow_header)
        self.assertIn("\n  schedule:", self.workflow_header)

    def test_newer_full_ci_runs_cancel_older_runs_for_the_same_ref(self) -> None:
        self.assertIn(
            "\n  group: full-ci::${{ github.workflow }}::${{ github.ref }}",
            self.workflow_header,
        )
        self.assertIn("\n  cancel-in-progress: true", self.workflow_header)

    def test_legacy_postmerge_entrypoint_is_removed(self) -> None:
        self.assertFalse(LEGACY_POSTMERGE_WORKFLOW.exists())

    def test_bounded_ci_still_runs_for_pull_requests_and_main_pushes(self) -> None:
        workflow_header = BLOCKING_CI_WORKFLOW.read_text().split(
            "\njobs:\n", maxsplit=1
        )[0]
        self.assertIn("\n  pull_request:", workflow_header)
        self.assertIn("\n  push:", workflow_header)
        self.assertIn("\n    branches: [main]", workflow_header)

    def test_opt_in_rust_full_ci_cancels_superseded_runs(self) -> None:
        workflow_header = RUST_FULL_CI_WORKFLOW.read_text().split(
            "\njobs:\n", maxsplit=1
        )[0]
        self.assertIn(
            "\n  group: rust-ci-full::${{ github.workflow }}::${{ github.ref }}",
            workflow_header,
        )
        self.assertIn("\n  cancel-in-progress: true", workflow_header)

    def test_scheduled_v8_canary_forces_a_complete_run(self) -> None:
        workflow = V8_CANARY_WORKFLOW.read_text()
        self.assertIn(
            '"${EVENT_NAME}" == "workflow_dispatch" || "${EVENT_NAME}" == "schedule"',
            workflow,
        )

    def test_codex_lab_release_requires_full_verification(self) -> None:
        workflow = CODEX_LAB_RELEASE_WORKFLOW.read_text()
        for job_name, workflow_name in (
            ("full-bazel", "bazel.yml"),
            ("full-rust", "rust-ci-full.yml"),
            ("full-sdk-integration", "sdk-integration.yml"),
            ("full-v8-canary", "v8-canary.yml"),
        ):
            with self.subTest(job=job_name):
                self.assertIn(
                    f"  {job_name}:\n"
                    "    name: Full verification / "
                    + {
                        "full-bazel": "Bazel",
                        "full-rust": "Rust",
                        "full-sdk-integration": "SDK integration",
                        "full-v8-canary": "V8 canary",
                    }[job_name]
                    + "\n"
                    "    needs: release-metadata\n"
                    f"    uses: ./.github/workflows/{workflow_name}\n"
                    "    secrets: inherit\n",
                    workflow,
                )
        self.assertIn(
            "  full-verification:\n"
            "    name: Full verification results\n"
            "    needs:\n"
            "      - full-bazel\n"
            "      - full-rust\n"
            "      - full-sdk-integration\n"
            "      - full-v8-canary\n",
            workflow,
        )
        self.assertIn("run: python3 .github/scripts/check_ci_results.py", workflow)
        self.assertIn(
            "  build-macos-aarch64:\n"
            "    name: Build macOS ARM64 Codex Lab release artifacts\n"
            "    needs:\n"
            "      - release-metadata\n"
            "      - full-verification\n",
            workflow,
        )

    def test_nightly_and_release_use_the_same_full_verification_components(self) -> None:
        self.assertEqual(called_workflows(FULL_CI_WORKFLOW), FULL_VERIFICATION_WORKFLOWS)
        release_calls = called_workflows(CODEX_LAB_RELEASE_WORKFLOW)
        self.assertTrue(FULL_VERIFICATION_WORKFLOWS.issubset(release_calls))

    def test_reusable_workflow_nesting_stays_within_github_limit(self) -> None:
        for workflow_path in (FULL_CI_WORKFLOW, CODEX_LAB_RELEASE_WORKFLOW):
            with self.subTest(workflow=workflow_path.name):
                self.assertLessEqual(reusable_workflow_depth(workflow_path), 4)

    def test_full_ci_components_fail_fast(self) -> None:
        for workflow_path in FULL_CI_COMPONENTS:
            with self.subTest(workflow=workflow_path.name):
                component = workflow_path.read_text()
                self.assertNotIn("fail-fast: false", component)
                self.assertNotIn("--no-fail-fast", component)
                self.assertNotIn("--keep_going", component)


if __name__ == "__main__":
    unittest.main()
