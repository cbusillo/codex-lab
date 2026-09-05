from pathlib import Path
import unittest


class SetupCiActionTests(unittest.TestCase):
    def test_self_hosted_linux_build_root_is_namespaced_by_runner(self) -> None:
        action = Path(".github/actions/setup-ci/action.yml").read_text(encoding="utf-8")

        self.assertIn("RUNNER_NAME_VALUE: ${{ runner.name }}", action)
        self.assertIn('ci_build_root="$HOME/.cache/codex-ci/$runner_slug"', action)
        self.assertIn('cache_scope="self-hosted-$runner_slug"', action)
        self.assertIn(
            'echo "SCCACHE_SERVER_PORT=$((20000 + runner_checksum % 10000))"',
            action,
        )
        self.assertIn('sccache_socket_dir="$HOME/.cache/codex-sccache"', action)
        self.assertIn(
            'echo "SCCACHE_SERVER_UDS=$sccache_socket_dir/$runner_checksum.sock"',
            action,
        )

    def test_external_cache_keys_include_the_runner_scope(self) -> None:
        prepare_bazel = Path(".github/actions/prepare-bazel-ci/action.yml").read_text(
            encoding="utf-8"
        )
        lint_build = Path(".github/workflows/rust-ci-full-lint-build.yml").read_text(
            encoding="utf-8"
        )
        nextest = Path(".github/workflows/rust-ci-full-nextest-platform.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn("bazel-cache-v2-${CACHE_RUNNER_SCOPE}", prepare_bazel)
        self.assertIn(
            "cargo-home-v2-${{ steps.setup_ci.outputs.cache-scope }}", lint_build
        )
        self.assertIn(
            "sccache-v2-${{ steps.setup_ci.outputs.cache-scope }}", lint_build
        )
        self.assertIn(
            "cargo-home-v2-${{ steps.setup_ci.outputs.cache-scope }}", nextest
        )
        self.assertIn("sccache-v2-${{ steps.setup_ci.outputs.cache-scope }}", nextest)

    def test_run_scoped_bazel_repository_contents_use_runner_temp(self) -> None:
        action = Path(".github/actions/setup-ci/action.yml").read_text(encoding="utf-8")

        self.assertIn(
            'bazel_repo_contents_cache="$RUNNER_TEMP/bazel-repo-contents-cache"',
            action,
        )
        self.assertNotIn(
            'bazel_repo_contents_cache="$CI_BUILD_ROOT/bazel-repo-contents-cache-',
            action,
        )
        self.assertIn(
            'bazel_repository_cache="$CI_BUILD_ROOT/bazel-repository-cache"', action
        )
        self.assertIn('cargo_target_dir="$CI_BUILD_ROOT/cargo-target"', action)

    def test_job_temporary_data_uses_ephemeral_os_storage(self) -> None:
        action = Path(".github/actions/setup-ci/action.yml").read_text(encoding="utf-8")

        self.assertIn('if [[ "${RUNNER_OS:-}" == "Windows" ]]', action)
        self.assertIn('tmp="$CI_BUILD_ROOT/tmp"', action)
        self.assertIn('job_tmp="$RUNNER_TEMP/codexcitemp"', action)
        self.assertIn('short_tmp="/tmp/codexci$runner_checksum"', action)
        self.assertIn('ln -sfn "$job_tmp" "$short_tmp"', action)
        self.assertIn('tmp="$short_tmp"', action)
        self.assertIn('tmp="$RUNNER_TEMP/codexcitemp"', action)


if __name__ == "__main__":
    unittest.main()
