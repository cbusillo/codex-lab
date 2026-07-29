import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
FULL_CI_WORKFLOW = ROOT / ".github/workflows/full-ci.yml"
LEGACY_POSTMERGE_WORKFLOW = ROOT / ".github/workflows/postmerge-ci.yml"
BLOCKING_CI_WORKFLOW = ROOT / ".github/workflows/blocking-ci.yml"
RUST_FULL_CI_WORKFLOW = ROOT / ".github/workflows/rust-ci-full.yml"
RUST_BLOCKING_CI_WORKFLOW = ROOT / ".github/workflows/rust-ci.yml"
V8_CANARY_WORKFLOW = ROOT / ".github/workflows/v8-canary.yml"
CODEX_LAB_RELEASE_WORKFLOW = ROOT / ".github/workflows/codex-lab-release.yml"
FULL_CI_MATRIX_WORKFLOWS = (
    ROOT / ".github/workflows/sdk-integration.yml",
    ROOT / ".github/workflows/bazel.yml",
    ROOT / ".github/workflows/rust-ci-full.yml",
    ROOT / ".github/workflows/rust-ci-full-nextest-platform.yml",
    ROOT / ".github/workflows/v8-canary.yml",
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

    def test_bounded_ci_compiles_the_rust_workspace(self) -> None:
        workflow = RUST_BLOCKING_CI_WORKFLOW.read_text()

        self.assertIn("  workspace_check:\n", workflow)
        self.assertIn("run: cargo check --workspace --tests --locked", workflow)
        self.assertIn("workspace_check,", workflow)

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

    def test_codex_lab_release_derives_artifact_version_from_tag(self) -> None:
        workflow = CODEX_LAB_RELEASE_WORKFLOW.read_text()

        self.assertIn(
            "      release_version: ${{ steps.release.outputs.release_version }}",
            workflow,
        )
        self.assertIn(
            "from codex_lab_package.release_tag import release_version_from_tag",
            workflow,
        )
        self.assertIn(
            "      CODE_VERSION: ${{ needs.release-metadata.outputs.release_version }}",
            workflow,
        )
        self.assertIn("Configure clean Codex Lab build provenance", workflow)
        self.assertIn(
            "git status --porcelain=v1 --untracked-files=normal --ignore-submodules=none",
            workflow,
        )
        self.assertIn('echo "CODEX_BUILD_CHANNEL=lab"', workflow)
        self.assertIn('echo "CODEX_BUILD_COMMIT=$(git rev-parse HEAD)"', workflow)
        self.assertIn('echo "CODEX_BUILD_DIRTY=clean"', workflow)
        self.assertIn(
            "cargo build --locked --timings --release -p codex-cli --bin codex-lab",
            workflow,
        )
        self.assertIn('lab_version="${CODE_VERSION:?missing release version}"', workflow)
        self.assertIn('--embedded-cli-version "$CODE_VERSION"', workflow)
        self.assertIn('expected_version = os.environ["CODE_VERSION"]', workflow)
        self.assertIn('if identity.build_channel != "lab":', workflow)
        self.assertIn('--version "$CODE_VERSION"', workflow)
        self.assertIn(
            '--arg version "${{ needs.build-macos-aarch64.outputs.release_version }}"',
            workflow,
        )
        self.assertLess(
            workflow.index("Fail fast when release tag is unavailable"),
            workflow.index("Full verification / Bazel"),
        )

    def test_nightly_and_release_use_the_same_full_verification_components(self) -> None:
        self.assertEqual(called_workflows(FULL_CI_WORKFLOW), FULL_VERIFICATION_WORKFLOWS)
        release_calls = called_workflows(CODEX_LAB_RELEASE_WORKFLOW)
        self.assertTrue(FULL_VERIFICATION_WORKFLOWS.issubset(release_calls))

    def test_reusable_workflow_nesting_stays_within_github_limit(self) -> None:
        for workflow_path in (FULL_CI_WORKFLOW, CODEX_LAB_RELEASE_WORKFLOW):
            with self.subTest(workflow=workflow_path.name):
                self.assertLessEqual(reusable_workflow_depth(workflow_path), 4)

    def test_full_ci_collects_complete_diagnostics(self) -> None:
        for workflow_path in FULL_CI_MATRIX_WORKFLOWS:
            with self.subTest(workflow=workflow_path.name):
                component = workflow_path.read_text()
                strategy_count = len(
                    re.findall(r"^\s+strategy:\s*$", component, flags=re.MULTILINE)
                )
                fail_fast_false_count = len(
                    re.findall(
                        r"^\s+fail-fast:\s+false\s*$",
                        component,
                        flags=re.MULTILINE,
                    )
                )
                self.assertEqual(strategy_count, fail_fast_false_count)

        self.assertIn("common:ci --keep_going", (ROOT / ".bazelrc").read_text())
        self.assertIn(
            "post_config_bazel_args=(--keep_going)",
            (ROOT / ".github/scripts/run-bazel-ci.sh").read_text(),
        )
        self.assertIn(
            "--no-fail-fast",
            (ROOT / ".github/workflows/rust-ci-full-nextest-platform.yml").read_text(),
        )

    def test_argument_comment_lint_has_bounded_local_fallback(self) -> None:
        workflow = RUST_FULL_CI_WORKFLOW.read_text()

        self.assertIn(
            'if [[ -z "${BUILDBUDDY_API_KEY}" && "${RUNNER_OS}" != "Windows" ]]',
            workflow,
        )
        self.assertIn(
            "python3 ./tools/argument-comment-lint/run-prebuilt-linter.py -- --ignore-rust-version",
            workflow,
        )
        self.assertIn("rustup toolchain install nightly-2025-09-18", workflow)
        self.assertIn("argument-comment-workspace-${{ runner.os }}", workflow)
        self.assertIn("uses: ./.github/actions/setup-rusty-v8", workflow)


if __name__ == "__main__":
    unittest.main()
