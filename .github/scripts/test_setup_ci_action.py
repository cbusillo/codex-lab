from pathlib import Path
import unittest


class SetupCiActionTests(unittest.TestCase):
    def test_self_hosted_linux_build_root_is_namespaced_by_runner(self) -> None:
        action = Path(".github/actions/setup-ci/action.yml").read_text(encoding="utf-8")

        self.assertIn("RUNNER_NAME_VALUE: ${{ runner.name }}", action)
        self.assertIn('ci_build_root="$HOME/.cache/codex-ci/$runner_slug"', action)

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
        self.assertIn('bazel_repository_cache="$CI_BUILD_ROOT/bazel-repository-cache"', action)
        self.assertIn('cargo_target_dir="$CI_BUILD_ROOT/cargo-target"', action)


if __name__ == "__main__":
    unittest.main()
