import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = REPO_ROOT / ".github/workflows/codex-lab-bazel-analysis.yml"
BLOCKING = REPO_ROOT / ".github/workflows/blocking-ci.yml"
PREPARE_ACTION = REPO_ROOT / ".github/actions/prepare-bazel-ci/action.yml"


class BazelAnalysisWorkflowTest(unittest.TestCase):
    def test_blocking_ci_routes_analysis_through_required_fan_in(self) -> None:
        contents = BLOCKING.read_text(encoding="utf-8")

        self.assertIn(
            "uses: ./.github/workflows/codex-lab-bazel-analysis.yml", contents
        )
        self.assertIn("- bazel-analysis", contents)

    def test_analysis_is_hosted_path_aware_and_nobuild(self) -> None:
        contents = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("runs-on: macos-26", contents)
        self.assertNotIn("self-hosted", contents)
        self.assertIn("--nobuild", contents)
        self.assertIn("scripts/list-bazel-release-targets.sh", contents)
        self.assertIn("elapsed_seconds > 1500", contents)
        self.assertIn("codex-rs/*", contents)
        self.assertIn("bazel/*", contents)
        self.assertIn("*.bzl", contents)
        self.assertIn('files=("__no_changes__")', contents)
        self.assertIn('changed_files="$(git diff', contents)
        self.assertIn("Unable to diff pull request base", contents)
        self.assertIn("timeout-minutes: 5", contents)
        self.assertIn("MODULE.bazel.lock", contents)
        self.assertNotIn("name: CI results (required)", contents)

    def test_repository_cache_failures_are_non_blocking(self) -> None:
        contents = WORKFLOW.read_text(encoding="utf-8")
        prepare_contents = PREPARE_ACTION.read_text(encoding="utf-8")

        self.assertIn("uses: ./.github/actions/prepare-bazel-ci", contents)
        self.assertIn("Restore bazel repository cache", prepare_contents)
        self.assertIn("continue-on-error: true", prepare_contents)
        self.assertIn("Save Bazel repository cache", contents)
        self.assertIn("continue-on-error: true", contents)


if __name__ == "__main__":
    unittest.main()
