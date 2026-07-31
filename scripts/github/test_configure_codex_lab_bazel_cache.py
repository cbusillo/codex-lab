#!/usr/bin/env python3
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "github" / "configure-codex-lab-bazel-cache.sh"


class ConfigureCodexLabBazelCacheTest(unittest.TestCase):
    def test_uses_configured_artifact_root_on_self_hosted_runner(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"
            summary_file = Path(tmp) / "summary"
            artifact_root = Path(tmp) / "artifact-root"
            artifact_root.mkdir()

            completed = run_helper(
                env_file,
                RUNNER_ENVIRONMENT="self-hosted",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                GITHUB_REPOSITORY="owner/repo",
                GITHUB_STEP_SUMMARY=str(summary_file),
            )

            cache_root = (
                artifact_root
                / "github-actions"
                / "cache"
                / "owner/repo"
                / "v8-canary"
            )
            disk_cache = cache_root / "bazel-disk-cache"
            repository_cache = cache_root / "bazel-repository-cache"
            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Using configured persistent Bazel caches", completed.stdout)
            self.assertTrue(disk_cache.is_dir())
            self.assertTrue(repository_cache.is_dir())
            self.assertEqual(
                f"BAZEL_DISK_CACHE={disk_cache}\n"
                "BAZEL_DISK_CACHE_GC_MAX_SIZE=100G\n"
                f"BAZEL_REPOSITORY_CACHE={repository_cache}\n"
                "CODEX_LAB_BAZEL_CACHE_MODE=persistent\n",
                env_file.read_text(),
            )
            self.assertIn("disk cache limit: `100G`", summary_file.read_text())
            self.assertNotIn(str(artifact_root), completed.stdout)
            self.assertNotIn(str(artifact_root), summary_file.read_text())

    def test_keeps_hosted_runner_cache_ephemeral(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"
            artifact_root = Path(tmp) / "artifact-root"
            artifact_root.mkdir()

            completed = run_helper(
                env_file,
                RUNNER_ENVIRONMENT="github-hosted",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Using setup-bazel-ci ephemeral caches", completed.stdout)
            self.assertEqual(
                "CODEX_LAB_BAZEL_CACHE_MODE=ephemeral\n", env_file.read_text()
            )
            self.assertFalse((artifact_root / "github-actions").exists())

    def test_rejects_unsafe_cache_leaf(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"

            completed = run_helper(env_file, script_args=("../escape",))

            self.assertEqual(2, completed.returncode)
            self.assertIn("invalid workflow cache leaf", completed.stderr)
            self.assertFalse(env_file.exists())

    def test_rejects_invalid_disk_cache_limit(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"

            completed = run_helper(
                env_file, script_args=("v8-canary", "unbounded")
            )

            self.assertEqual(2, completed.returncode)
            self.assertIn("invalid Bazel disk cache maximum size", completed.stderr)
            self.assertFalse(env_file.exists())


def run_helper(
    env_file: Path,
    script_args: tuple[str, ...] = ("v8-canary",),
    **overrides: str,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    for name in (
        "CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT",
        "GITHUB_STEP_SUMMARY",
        "RUNNER_ENVIRONMENT",
    ):
        env.pop(name, None)
    env.update(
        {
            "GITHUB_ENV": str(env_file),
            "GITHUB_REPOSITORY": "cbusillo/codex-lab",
        }
    )
    env.update(overrides)
    return subprocess.run(
        [str(SCRIPT), *script_args],
        check=False,
        env=env,
        stderr=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )


if __name__ == "__main__":
    unittest.main()
