import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
FULL_CI_WORKFLOW = ROOT / ".github/workflows/full-ci.yml"
LEGACY_POSTMERGE_WORKFLOW = ROOT / ".github/workflows/postmerge-ci.yml"
BLOCKING_CI_WORKFLOW = ROOT / ".github/workflows/blocking-ci.yml"
RUST_FULL_CI_WORKFLOW = ROOT / ".github/workflows/rust-ci-full.yml"
V8_CANARY_WORKFLOW = ROOT / ".github/workflows/v8-canary.yml"
FULL_CI_COMPONENTS = (
    ROOT / ".bazelrc",
    ROOT / ".github/workflows/sdk-integration.yml",
    ROOT / ".github/workflows/bazel.yml",
    ROOT / ".github/workflows/rust-ci-full.yml",
    ROOT / ".github/workflows/rust-ci-full-nextest-platform.yml",
    ROOT / ".github/workflows/v8-canary.yml",
    ROOT / ".github/scripts/run-argument-comment-lint-bazel.sh",
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
        self.assertIn("\n  group: full-ci::${{ github.ref }}", self.workflow_header)
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
        self.assertIn("\n  group: rust-ci-full::${{ github.ref }}", workflow_header)
        self.assertIn("\n  cancel-in-progress: true", workflow_header)

    def test_scheduled_v8_canary_forces_a_complete_run(self) -> None:
        workflow = V8_CANARY_WORKFLOW.read_text()
        self.assertIn(
            '"${EVENT_NAME}" == "workflow_dispatch" || "${EVENT_NAME}" == "schedule"',
            workflow,
        )

    def test_full_ci_components_fail_fast(self) -> None:
        for workflow_path in FULL_CI_COMPONENTS:
            with self.subTest(workflow=workflow_path.name):
                component = workflow_path.read_text()
                self.assertNotIn("fail-fast: false", component)
                self.assertNotIn("--no-fail-fast", component)
                self.assertNotIn("--keep_going", component)


if __name__ == "__main__":
    unittest.main()
