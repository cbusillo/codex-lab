import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
FULL_CI_WORKFLOW = ROOT / ".github/workflows/full-ci.yml"
LEGACY_POSTMERGE_WORKFLOW = ROOT / ".github/workflows/postmerge-ci.yml"
BLOCKING_CI_WORKFLOW = ROOT / ".github/workflows/blocking-ci.yml"
RUST_FULL_CI_WORKFLOW = ROOT / ".github/workflows/rust-ci-full.yml"
RUST_WINDOWS_FULL_CI_WORKFLOW = ROOT / ".github/workflows/rust-ci-full-windows.yml"
RUST_ARGUMENT_COMMENT_LINT_WORKFLOW = (
    ROOT / ".github/workflows/rust-ci-full-argument-comment-lint.yml"
)
RUST_NEXTEST_PLATFORM_WORKFLOW = (
    ROOT / ".github/workflows/rust-ci-full-nextest-platform.yml"
)
RUST_BLOCKING_CI_WORKFLOW = ROOT / ".github/workflows/rust-ci.yml"
BAZEL_WORKFLOW = ROOT / ".github/workflows/bazel.yml"
V8_CANARY_WORKFLOW = ROOT / ".github/workflows/v8-canary.yml"
V8_CANARY_WINDOWS_WORKFLOW = ROOT / ".github/workflows/v8-canary-windows.yml"
V8_CANARY_METADATA_WORKFLOW = ROOT / ".github/workflows/v8-canary-metadata.yml"
CODEX_LAB_RELEASE_WORKFLOW = ROOT / ".github/workflows/codex-lab-release.yml"
CODEX_LAB_APP_WORKFLOW = ROOT / ".github/workflows/codex-lab-app.yml"
SETUP_RUSTY_V8_ACTION = ROOT / ".github/actions/setup-rusty-v8/action.yml"
FULL_CI_MATRIX_WORKFLOWS = (
    ROOT / ".github/workflows/sdk-integration.yml",
    ROOT / ".github/workflows/bazel.yml",
    ROOT / ".github/workflows/rust-ci-full.yml",
    ROOT / ".github/workflows/rust-ci-full-nextest-platform.yml",
    ROOT / ".github/workflows/v8-canary.yml",
)
RELEASE_VERIFICATION_WORKFLOWS = {
    "bazel.yml",
    "rust-ci-full.yml",
    "sdk-integration.yml",
    "v8-canary.yml",
}
WINDOWS_FULL_VERIFICATION_WORKFLOWS = {
    "rust-ci-full-windows.yml",
    "v8-canary-windows.yml",
}
FULL_VERIFICATION_WORKFLOWS = RELEASE_VERIFICATION_WORKFLOWS
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
        workflow = V8_CANARY_METADATA_WORKFLOW.read_text()
        self.assertIn(
            '"${EVENT_NAME}" == "workflow_dispatch" || "${EVENT_NAME}" == "schedule"',
            workflow,
        )
        self.assertIn(
            "windows_source_required: ${{ steps.changes.outputs.windows_source_required || 'true' }}",
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
                permissions = (
                    "    permissions:\n"
                    "      contents: read\n"
                    "      actions: read\n"
                    if job_name == "full-v8-canary"
                    else ""
                )
                job_block = (
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
                    + permissions
                    + f"    uses: ./.github/workflows/{workflow_name}\n"
                    "    secrets: inherit\n"
                )
                self.assertIn(job_block, workflow)
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
        self.assertIn(
            "      CODEX_LAB_RELEASE_VERSION: ${{ needs.release-metadata.outputs.release_identity }}",
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
        self.assertIn(
            'lab_version="${CODE_VERSION:?missing compatibility version}"', workflow
        )
        self.assertIn(
            'release_version="${CODEX_LAB_RELEASE_VERSION:?missing release identity}"',
            workflow,
        )
        self.assertIn('--embedded-cli-version "$release_version"', workflow)
        self.assertIn('expected_version = os.environ["CODE_VERSION"]', workflow)
        self.assertIn(
            'expected_version="codex-lab ${CODEX_LAB_RELEASE_VERSION:?missing release identity}"',
            workflow,
        )
        self.assertIn('--arg release "$CODEX_LAB_RELEASE_VERSION"', workflow)
        self.assertIn('if identity.build_channel != "lab":', workflow)
        self.assertIn('--version "$CODE_VERSION"', workflow)
        self.assertIn('--release-version "$CODEX_LAB_RELEASE_VERSION"', workflow)
        self.assertIn(
            '--arg version "${{ needs.build-macos-aarch64.outputs.release_version }}"',
            workflow,
        )
        self.assertLess(
            workflow.index("Fail fast when release tag is unavailable"),
            workflow.index("Full verification / Bazel"),
        )

    def test_full_ci_and_release_use_the_same_apple_silicon_suites(self) -> None:
        self.assertEqual(called_workflows(FULL_CI_WORKFLOW), FULL_VERIFICATION_WORKFLOWS)
        release_calls = called_workflows(CODEX_LAB_RELEASE_WORKFLOW)
        self.assertEqual(
            release_calls,
            RELEASE_VERIFICATION_WORKFLOWS | {"authorize-self-hosted.yml"},
        )
        self.assertTrue(WINDOWS_FULL_VERIFICATION_WORKFLOWS.isdisjoint(release_calls))

    def test_release_verification_workflows_have_no_windows_jobs(self) -> None:
        release_rust = RUST_FULL_CI_WORKFLOW.read_text()
        windows_rust = RUST_WINDOWS_FULL_CI_WORKFLOW.read_text()
        release_v8 = V8_CANARY_WORKFLOW.read_text()
        windows_v8 = V8_CANARY_WINDOWS_WORKFLOW.read_text()

        for marker in ("windows-2025", "windows-11-arm", "windows-msvc"):
            with self.subTest(marker=marker):
                self.assertNotIn(marker, release_rust)
                self.assertIn(marker, windows_rust)
        self.assertNotIn("build-windows-source", release_v8)
        self.assertIn("build-windows-source", windows_v8)

    def test_v8_callers_allow_nested_actions_reads(self) -> None:
        permissions = (
            "    permissions:\n"
            "      contents: read\n"
            "      actions: read\n"
            "    uses: ./.github/workflows/v8-canary.yml\n"
        )

        self.assertIn(permissions, FULL_CI_WORKFLOW.read_text())
        self.assertIn(permissions, CODEX_LAB_RELEASE_WORKFLOW.read_text())

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
            RUST_NEXTEST_PLATFORM_WORKFLOW.read_text(),
        )

    def test_bazel_jobs_have_cold_cache_headroom(self) -> None:
        workflow = BAZEL_WORKFLOW.read_text()

        self.assertEqual(workflow.count("timeout-minutes: 90"), 3)
        self.assertNotIn("timeout-minutes: 60", workflow)

    def test_macos_aarch64_runs_the_native_nextest_suite(self) -> None:
        workflow = RUST_FULL_CI_WORKFLOW.read_text()
        platform_workflow = RUST_NEXTEST_PLATFORM_WORKFLOW.read_text()

        self.assertIn("  tests_macos_aarch64:\n", workflow)
        self.assertIn("      runner: macos-26\n", workflow)
        self.assertIn("      target: aarch64-apple-darwin\n", workflow)
        self.assertNotIn("remote_test_filter:", workflow)
        self.assertIn('run_nextest "${nextest_args[@]}"', platform_workflow)

    def test_argument_comment_lint_has_bounded_local_fallback(self) -> None:
        workflow = RUST_ARGUMENT_COMMENT_LINT_WORKFLOW.read_text()

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

    def test_argument_comment_lint_workflow_changes_invalidate_ci_state(self) -> None:
        workflow_path = ".github/workflows/rust-ci-full-argument-comment-lint.yml"
        blocking_workflow = RUST_BLOCKING_CI_WORKFLOW.read_text()
        full_workflow = RUST_FULL_CI_WORKFLOW.read_text()

        self.assertIn(f"$f == {workflow_path}", blocking_workflow)
        self.assertIn(f"'{workflow_path}'", blocking_workflow)
        self.assertIn(f"'{workflow_path}'", full_workflow)

    def test_codex_lab_app_tracks_rusty_v8_consumer_inputs(self) -> None:
        workflow = CODEX_LAB_APP_WORKFLOW.read_text()

        for path in (
            ".github/actions/setup-rusty-v8/**",
            ".github/scripts/run_bazel_with_buildbuddy.py",
            ".github/scripts/rusty_v8_bazel.py",
            ".github/scripts/rusty_v8_module_bazel.py",
            "third_party/v8/rusty_v8_*_codex_release.sha256",
        ):
            with self.subTest(path=path):
                self.assertIn(f'- "{path}"', workflow)

    def test_rusty_v8_consumers_use_reviewed_release_checksums(self) -> None:
        action = SETUP_RUSTY_V8_ACTION.read_text()
        release_workflow = CODEX_LAB_RELEASE_WORKFLOW.read_text()

        self.assertIn("write-release-checksums", action)
        self.assertNotIn(
            'curl -fsSL "${base_url}/${checksums_name}"',
            action,
        )
        self.assertIn("uses: ./.github/actions/setup-rusty-v8", release_workflow)

    def test_rust_ci_tracks_rusty_v8_consumer_inputs(self) -> None:
        workflow = RUST_BLOCKING_CI_WORKFLOW.read_text()

        for path in (
            ".github/actions/setup-rusty-v8/*",
            ".github/scripts/run_bazel_with_buildbuddy.py",
            ".github/scripts/rusty_v8_bazel.py",
            ".github/scripts/rusty_v8_module_bazel.py",
            "third_party/v8/rusty_v8_*_codex_release.sha256",
        ):
            with self.subTest(path=path):
                self.assertIn(f"$f == {path}", workflow)


if __name__ == "__main__":
    unittest.main()
