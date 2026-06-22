#!/usr/bin/env python3
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class LocalCleanupSpaceTest(unittest.TestCase):
    def setUp(self) -> None:
        if shutil.which("just") is None:
            self.skipTest("just is required for local-cleanup-space recipe tests")

    def test_local_cleanup_space_dry_run_keeps_paths(self) -> None:
        with copied_cleanup_workspace() as workspace:
            completed = run_local_cleanup(workspace)

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Local space cleanup (dry run)", completed.stdout)
            self.assertTrue((workspace / "codex-rs" / "target").exists())
            self.assertTrue((workspace / ".tmp" / "codex-exec-harness").exists())
            self.assertTrue((workspace / ".tmp" / "codex-exec-harness-ci").exists())

    def test_local_cleanup_space_apply_deletes_paths(self) -> None:
        with copied_cleanup_workspace() as workspace:
            completed = run_local_cleanup(workspace, "--apply")

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Local space cleanup (apply)", completed.stdout)
            self.assertFalse((workspace / "codex-rs" / "target").exists())
            self.assertFalse((workspace / ".tmp" / "codex-exec-harness").exists())
            self.assertFalse((workspace / ".tmp" / "codex-exec-harness-ci").exists())
            self.assertFalse((workspace / "exec-harness-target").exists())

    def test_local_cleanup_space_can_preserve_exec_harness_cache(self) -> None:
        with copied_cleanup_workspace() as workspace:
            completed = run_local_cleanup(
                workspace, "--apply", "--keep-exec-harness-cache"
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Local space cleanup (apply)", completed.stdout)
            self.assertFalse((workspace / "codex-rs" / "target").exists())
            self.assertFalse((workspace / ".tmp" / "codex-exec-harness").exists())
            self.assertFalse((workspace / ".tmp" / "codex-exec-harness-ci").exists())
            self.assertTrue((workspace / "exec-harness-target").exists())

    def test_local_cleanup_space_can_preserve_local_cargo_cache(self) -> None:
        with copied_cleanup_workspace() as workspace:
            completed = run_local_cleanup(
                workspace, "--apply", "--keep-local-cargo-cache"
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Local space cleanup (apply)", completed.stdout)
            self.assertFalse((workspace / "codex-rs" / "target").exists())
            self.assertFalse((workspace / ".tmp" / "codex-exec-harness").exists())
            self.assertFalse((workspace / ".tmp" / "codex-exec-harness-ci").exists())
            self.assertFalse((workspace / "exec-harness-target").exists())
            self.assertTrue((workspace / "local-cargo-target").exists())

    def test_cargo_build_env_prefers_explicit_target(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_cargo_build_env(
                workspace,
                CODEX_LAB_CARGO_TARGET_DIR=str(workspace / "explicit-target"),
            )

            self.assertEqual(
                f"export CARGO_TARGET_DIR={workspace / 'explicit-target'}\n",
                env.stdout,
            )
            self.assertTrue((workspace / "explicit-target").exists())

    def test_cargo_build_env_respects_existing_cargo_target_dir(self) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            artifact_root.mkdir()

            env = run_cargo_build_env(
                workspace,
                CARGO_TARGET_DIR=str(workspace / "existing-target"),
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(
                f"export CARGO_TARGET_DIR={workspace / 'existing-target'}\n",
                env.stdout,
            )
            self.assertTrue((workspace / "existing-target").exists())
            self.assertFalse((artifact_root / "local").exists())

    def test_cargo_build_env_resolves_relative_cargo_target_dir(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_cargo_build_env(
                workspace,
                CARGO_TARGET_DIR="relative-target",
                CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
            )

            self.assertEqual(
                f"export CARGO_TARGET_DIR={workspace / 'relative-target'}\n",
                env.stdout,
            )

    def test_cargo_build_env_falls_back_to_worktree_target(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_cargo_build_env(
                workspace,
                CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(workspace / "missing-volume"),
            )

            self.assertEqual(
                f"export CARGO_TARGET_DIR={workspace / 'codex-rs' / 'target'}\n",
                env.stdout,
            )

    def test_cargo_build_env_without_artifact_root_uses_worktree_target(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_cargo_build_env(
                workspace,
                CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
            )

            self.assertEqual(
                f"export CARGO_TARGET_DIR={workspace / 'codex-rs' / 'target'}\n",
                env.stdout,
            )

    def test_cargo_build_env_uses_canonical_repo_cache_name(self) -> None:
        with copied_cleanup_workspace() as workspace:
            subprocess.run(
                ["git", "init", "--quiet"],
                check=True,
                cwd=workspace,
            )
            subprocess.run(
                [
                    "git",
                    "config",
                    "remote.origin.url",
                    "git@github.com:cbusillo/codex-lab.git",
                ],
                check=True,
                cwd=workspace,
            )
            artifact_root = workspace / "artifact-root"
            artifact_root.mkdir()

            env = run_cargo_build_env(
                workspace,
                CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, env.returncode, env.stderr)
            self.assertIn(
                f"export CARGO_TARGET_DIR={artifact_root / 'local' / 'codex-lab' / 'cargo-target'}/",
                env.stdout,
            )
            self.assertNotIn(f"/local/{workspace.name}/", env.stdout)

    def test_exec_harness_env_prefers_explicit_target(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_exec_harness_env(
                workspace,
                CODEX_EXEC_HARNESS_CARGO_TARGET_DIR=str(workspace / "explicit-target"),
            )

            self.assertEqual(
                f"export CARGO_TARGET_DIR={workspace / 'explicit-target'}\n",
                env.stdout,
            )
            self.assertTrue((workspace / "explicit-target").exists())

    def test_exec_harness_env_respects_existing_cargo_target_dir(self) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            artifact_root.mkdir()

            env = run_exec_harness_env(
                workspace,
                CARGO_TARGET_DIR=str(workspace / "existing-target"),
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(
                f"export CARGO_TARGET_DIR={workspace / 'existing-target'}\n",
                env.stdout,
            )
            self.assertTrue((workspace / "existing-target").exists())
            self.assertFalse((artifact_root / "local").exists())

    def test_exec_harness_env_uses_artifact_root_when_configured(self) -> None:
        with copied_cleanup_workspace() as workspace:
            subprocess.run(
                ["git", "init", "--quiet"],
                check=True,
                cwd=workspace,
            )
            subprocess.run(
                [
                    "git",
                    "config",
                    "remote.origin.url",
                    "git@github.com:cbusillo/codex-lab.git",
                ],
                check=True,
                cwd=workspace,
            )
            artifact_root = workspace / "artifact-root"
            artifact_root.mkdir()

            env = run_exec_harness_env(
                workspace,
                CODEX_EXEC_HARNESS_NO_MKDIR="1",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, env.returncode, env.stderr)
            self.assertIn(
                f"export CARGO_TARGET_DIR={artifact_root / 'local' / 'codex-lab' / 'exec-harness' / 'cargo-target'}/",
                env.stdout,
            )
            self.assertNotIn(f"/local/{workspace.name}/", env.stdout)

    def test_exec_harness_env_without_artifact_root_uses_home_fallback(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_exec_harness_env(
                workspace,
                CODEX_EXEC_HARNESS_NO_MKDIR="1",
                HOME=str(workspace / "home"),
            )

            self.assertEqual(
                f"export CARGO_TARGET_DIR={workspace / 'home' / '.codex-lab' / 'working' / '_target-cache' / 'codex-lab' / 'exec-harness'}\n",
                env.stdout,
            )

    def test_speed_status_reports_unconfigured_artifact_root(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = os.environ.copy()
            env.pop("CARGO_TARGET_DIR", None)
            env.pop("CODEX_LAB_CARGO_TARGET_DIR", None)
            env.pop("CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT", None)
            env.pop("CODEX_LAB_REMOTE_COMPILE_HOST", None)

            completed = subprocess.run(
                [str(workspace / "scripts" / "local" / "speed-status.sh")],
                check=False,
                env=env,
                stderr=subprocess.PIPE,
                stdout=subprocess.PIPE,
                text=True,
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Artifact Root\nroot=not configured", completed.stdout)
            self.assertIn("Remote Compile Host\nhost=not configured", completed.stdout)


def run_local_cleanup(workspace: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["CODEX_EXEC_HARNESS_CARGO_TARGET_DIR"] = str(
        workspace / "exec-harness-target"
    )
    env["CODEX_LAB_CARGO_TARGET_DIR"] = str(workspace / "local-cargo-target")
    return subprocess.run(
        [
            "just",
            "--justfile",
            str(workspace / "justfile"),
            "--working-directory",
            str(workspace),
            "local-cleanup-space",
            *args,
        ],
        check=False,
        env=env,
        stderr=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )


def run_cargo_build_env(
    workspace: Path, **env_overrides: str
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.pop("CARGO_TARGET_DIR", None)
    env.pop("CODEX_LAB_CARGO_TARGET_DIR", None)
    env.update(env_overrides)
    return subprocess.run(
        [str(workspace / "scripts" / "local" / "cargo-build-env.sh")],
        check=False,
        env=env,
        stderr=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )


def run_exec_harness_env(
    workspace: Path, **env_overrides: str
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.pop("CARGO_TARGET_DIR", None)
    env.pop("CODEX_EXEC_HARNESS_CARGO_TARGET_DIR", None)
    env.pop("CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT", None)
    env.pop("CODEX_LAB_HOME", None)
    env.update(env_overrides)
    return subprocess.run(
        [str(workspace / "scripts" / "local" / "exec-harness-env.sh")],
        check=False,
        env=env,
        stderr=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )


class copied_cleanup_workspace:
    def __enter__(self) -> Path:
        self._temp_dir = tempfile.TemporaryDirectory()
        workspace = Path(self._temp_dir.name)

        copy_file("justfile", workspace)
        copy_file("scripts/just-shell.py", workspace)
        copy_file("scripts/local/cleanup-space.sh", workspace)
        copy_file("scripts/local/cargo-build-env.sh", workspace)
        copy_file("scripts/local/exec-harness-env.sh", workspace)
        copy_file("scripts/local/speed-status.sh", workspace)

        make_probe_dir(workspace / "codex-rs" / "target")
        make_probe_dir(workspace / ".tmp" / "codex-exec-harness")
        make_probe_dir(workspace / ".tmp" / "codex-exec-harness-ci")
        make_probe_dir(workspace / "exec-harness-target")
        make_probe_dir(workspace / "local-cargo-target")

        return workspace

    def __exit__(self, *args: object) -> None:
        self._temp_dir.cleanup()


def copy_file(relative_path: str, workspace: Path) -> None:
    source = REPO_ROOT / relative_path
    destination = workspace / relative_path
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


def make_probe_dir(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)


if __name__ == "__main__":
    unittest.main()
