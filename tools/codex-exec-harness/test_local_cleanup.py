#!/usr/bin/env python3
import os
import shlex
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
            self.assertFalse((workspace / ".tmp" / "exec-harness-target").exists())

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
            self.assertTrue((workspace / ".tmp" / "exec-harness-target").exists())

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
            self.assertFalse((workspace / ".tmp" / "exec-harness-target").exists())
            self.assertTrue((workspace / ".tmp" / "local-cargo-target").exists())

    def test_local_cleanup_space_skips_unbounded_exec_harness_cache(self) -> None:
        with copied_cleanup_workspace() as workspace:
            target = workspace / "custom-exec-target"
            make_probe_dir(target)

            completed = run_local_cleanup(
                workspace,
                "--apply",
                CODEX_EXEC_HARNESS_CARGO_TARGET_DIR=str(target),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("custom exec harness target cache", completed.stdout)
            self.assertTrue(target.exists())

    def test_local_cleanup_space_skips_unbounded_local_cargo_cache(self) -> None:
        with copied_cleanup_workspace() as workspace:
            target = workspace / "custom-cargo-target"
            make_probe_dir(target)

            completed = run_local_cleanup(
                workspace,
                "--apply",
                CODEX_LAB_CARGO_TARGET_DIR=str(target),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("custom local Cargo target cache", completed.stdout)
            self.assertTrue(target.exists())

    def test_local_cleanup_space_deletes_artifact_temp(self) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            artifact_tmp = artifact_root / "local" / "codex-lab" / "tmp"
            make_probe_dir(artifact_tmp)

            completed = run_local_cleanup(
                workspace,
                "--apply",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("artifact-volume temporary output", completed.stdout)
            self.assertFalse(artifact_tmp.exists())

    def test_local_cleanup_space_can_preserve_artifact_temp(self) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            artifact_tmp = artifact_root / "local" / "codex-lab" / "tmp"
            make_probe_dir(artifact_tmp)

            completed = run_local_cleanup(
                workspace,
                "--apply",
                "--keep-artifact-temp",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertTrue(artifact_tmp.exists())

    def test_local_cleanup_space_rejects_artifact_temp_symlink_escape(self) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            escaped_tmp = workspace / "escaped-tmp"
            make_probe_dir(escaped_tmp)
            make_probe_dir(artifact_root)
            (artifact_root / "local").symlink_to(
                escaped_tmp,
                target_is_directory=True,
            )
            make_probe_dir(escaped_tmp / "codex-lab" / "tmp")

            completed = run_local_cleanup(
                workspace,
                "--apply",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("artifact temp outside expected artifact root", completed.stdout)
            self.assertTrue(escaped_tmp.exists())

    def test_local_cleanup_space_skips_unbounded_exec_harness_output(self) -> None:
        with copied_cleanup_workspace() as workspace:
            with tempfile.TemporaryDirectory() as external_dir:
                output_root = Path(external_dir) / "custom-output"
                make_probe_dir(output_root)

                completed = run_local_cleanup(
                    workspace,
                    "--apply",
                    CODEX_EXEC_HARNESS_OUTPUT_ROOT=str(output_root),
                )

                self.assertEqual(0, completed.returncode, completed.stderr)
                self.assertIn("custom exec harness output root", completed.stdout)
                self.assertTrue(output_root.exists())

    def test_local_cleanup_space_rejects_repo_output_traversal(self) -> None:
        with copied_cleanup_workspace() as workspace:
            output_root = workspace / ".tmp" / "codex-exec-harness" / ".." / ".." / "codex-rs"

            completed = run_local_cleanup(
                workspace,
                "--apply",
                CODEX_EXEC_HARNESS_OUTPUT_ROOT=str(output_root),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertTrue((workspace / "codex-rs").exists())

    def test_local_cleanup_space_rejects_artifact_output_traversal(self) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            output_root = (
                artifact_root
                / "local"
                / "codex-lab"
                / "exec-harness"
                / "output"
                / ".."
                / ".."
                / "bazel"
            )
            make_probe_dir(artifact_root / "local" / "codex-lab" / "bazel")

            completed = run_local_cleanup(
                workspace,
                "--apply",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                CODEX_EXEC_HARNESS_OUTPUT_ROOT=str(output_root),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertTrue((artifact_root / "local" / "codex-lab" / "bazel").exists())

    def test_local_cleanup_space_deletes_bounded_artifact_output(self) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            output_root = (
                artifact_root / "local" / "codex-lab" / "exec-harness" / "output"
            )
            make_probe_dir(output_root)

            completed = run_local_cleanup(
                workspace,
                "--apply",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                CODEX_EXEC_HARNESS_OUTPUT_ROOT=str(output_root),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("exec harness configured output root", completed.stdout)
            self.assertFalse(output_root.exists())

    def test_local_cleanup_space_reports_existing_unbounded_output_root(self) -> None:
        with copied_cleanup_workspace() as workspace:
            output_root = workspace / "custom-output"
            make_probe_dir(output_root)

            completed = run_local_cleanup(
                workspace,
                "--apply",
                CODEX_EXEC_HARNESS_OUTPUT_ROOT=str(output_root),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("custom exec harness output root", completed.stdout)
            self.assertTrue(output_root.exists())

    def test_cargo_build_env_prefers_explicit_target(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_cargo_build_env(
                workspace,
                CODEX_LAB_CARGO_TARGET_DIR=str(workspace / "explicit-target"),
            )

            self.assertEqual(
                f"{workspace / 'explicit-target'}\n",
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
                f"{workspace / 'existing-target'}\n",
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
                f"{workspace / 'relative-target'}\n",
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
                f"{workspace / 'codex-rs' / 'target'}\n",
                env.stdout,
            )

    def test_cargo_build_env_without_artifact_root_uses_worktree_target(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_cargo_build_env(
                workspace,
                CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
            )

            self.assertEqual(
                f"{workspace / 'codex-rs' / 'target'}\n",
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
                f"{artifact_root / 'local' / 'codex-lab' / 'worktrees'}/",
                env.stdout,
            )
            self.assertNotIn(f"/local/{workspace.name}/", env.stdout)

    def test_cargo_build_env_worktree_scope_uses_worktree_namespace(self) -> None:
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
                CODEX_LAB_CARGO_TARGET_SCOPE="worktree",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, env.returncode, env.stderr)
            self.assertIn(
                f"{artifact_root / 'local' / 'codex-lab' / 'worktrees'}/",
                env.stdout,
            )
            self.assertIn("/cargo-target/", env.stdout)
            self.assertNotIn("/local/codex-lab/cargo-target/", env.stdout)

    def test_cargo_build_env_worktree_scope_is_unique_per_workspace(self) -> None:
        with copied_cleanup_workspace() as workspace_one:
            with copied_cleanup_workspace() as workspace_two:
                artifact_root = workspace_one / "artifact-root"
                artifact_root.mkdir()

                env_one = run_cargo_build_env(
                    workspace_one,
                    CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
                    CODEX_LAB_CARGO_TARGET_SCOPE="worktree",
                    CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                )
                env_two = run_cargo_build_env(
                    workspace_two,
                    CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
                    CODEX_LAB_CARGO_TARGET_SCOPE="worktree",
                    CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                )

                self.assertEqual(0, env_one.returncode, env_one.stderr)
                self.assertEqual(0, env_two.returncode, env_two.stderr)
                self.assertNotEqual(env_one.stdout, env_two.stdout)

    def test_cargo_build_env_worktree_scope_accepts_explicit_key(self) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            artifact_root.mkdir()

            env = run_cargo_build_env(
                workspace,
                CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
                CODEX_LAB_CARGO_TARGET_KEY="agent/session one",
                CODEX_LAB_CARGO_TARGET_SCOPE="agent",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, env.returncode, env.stderr)
            self.assertIn("/worktrees/agent-session-one/cargo-target/", env.stdout)

    def test_cargo_build_env_explicit_key_does_not_escape_worktree_namespace(
        self,
    ) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            artifact_root.mkdir()

            env = run_cargo_build_env(
                workspace,
                CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
                CODEX_LAB_CARGO_TARGET_KEY="..",
                CODEX_LAB_CARGO_TARGET_SCOPE="agent",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, env.returncode, env.stderr)
            self.assertIn("/worktrees/workspace/cargo-target/", env.stdout)
            self.assertNotIn("/worktrees/../cargo-target/", env.stdout)

    def test_cargo_build_env_warns_when_scope_is_shadowed_by_cargo_target(
        self,
    ) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            artifact_root.mkdir()

            env = run_cargo_build_env(
                workspace,
                CARGO_TARGET_DIR=str(workspace / "existing-target"),
                CODEX_LAB_CARGO_TARGET_SCOPE="agent",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertEqual(0, env.returncode, env.stderr)
            self.assertEqual(
                f"{workspace / 'existing-target'}\n",
                env.stdout,
            )
            self.assertIn("CARGO_TARGET_DIR is already set", env.stderr)

    def test_cargo_build_env_rejects_unknown_scope(self) -> None:
        with copied_cleanup_workspace() as workspace:
            artifact_root = workspace / "artifact-root"
            artifact_root.mkdir()

            env = run_cargo_build_env(
                workspace,
                CODEX_LAB_CARGO_TARGET_NO_MKDIR="1",
                CODEX_LAB_CARGO_TARGET_SCOPE="surprise",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
            )

            self.assertNotEqual(0, env.returncode)
            self.assertIn("unsupported CODEX_LAB_CARGO_TARGET_SCOPE", env.stderr)

    def test_exec_harness_env_prefers_explicit_target(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_exec_harness_env(
                workspace,
                CODEX_EXEC_HARNESS_CARGO_TARGET_DIR=str(workspace / "explicit-target"),
            )

            self.assertEqual(
                str(workspace / "explicit-target"),
                exported_value(env.stdout, "CARGO_TARGET_DIR"),
            )
            self.assertEqual(
                str(workspace / ".tmp" / "codex-exec-harness"),
                exported_value(env.stdout, "CODEX_EXEC_HARNESS_OUTPUT_ROOT"),
            )
            self.assertEqual(
                str(workspace / ".tmp" / "codex-exec-harness" / "report.json"),
                exported_value(env.stdout, "CODEX_EXEC_HARNESS_REPORT_JSON"),
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
                str(workspace / "existing-target"),
                exported_value(env.stdout, "CARGO_TARGET_DIR"),
            )
            self.assertIn(
                f"{artifact_root / 'local'}/",
                exported_value(env.stdout, "CODEX_EXEC_HARNESS_OUTPUT_ROOT"),
            )
            self.assertTrue(
                exported_value(env.stdout, "CODEX_EXEC_HARNESS_OUTPUT_ROOT").endswith(
                    "/exec-harness/output"
                )
            )
            self.assertTrue((workspace / "existing-target").exists())
            self.assertTrue((artifact_root / "local").exists())

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
            self.assertEqual(
                str(artifact_root / "local" / "codex-lab" / "exec-harness" / "output"),
                exported_value(env.stdout, "CODEX_EXEC_HARNESS_OUTPUT_ROOT"),
            )
            self.assertEqual(
                str(
                    artifact_root
                    / "local"
                    / "codex-lab"
                    / "exec-harness"
                    / "output"
                    / "report.json"
                ),
                exported_value(env.stdout, "CODEX_EXEC_HARNESS_REPORT_JSON"),
            )
            self.assertNotIn(f"/local/{workspace.name}/", env.stdout)

    def test_exec_harness_env_without_artifact_root_uses_home_fallback(self) -> None:
        with copied_cleanup_workspace() as workspace:
            env = run_exec_harness_env(
                workspace,
                CODEX_EXEC_HARNESS_NO_MKDIR="1",
                HOME=str(workspace / "home"),
            )

            self.assertTrue(
                exported_value(env.stdout, "CARGO_TARGET_DIR").startswith(
                    str(workspace / "home" / ".codex-lab" / "working" / "_target-cache")
                ),
                env.stdout,
            )
            self.assertIn("/exec-harness", exported_value(env.stdout, "CARGO_TARGET_DIR"))
            self.assertEqual(
                str(workspace / ".tmp" / "codex-exec-harness"),
                exported_value(env.stdout, "CODEX_EXEC_HARNESS_OUTPUT_ROOT"),
            )

    def test_exec_harness_env_uses_unique_name_without_git_metadata(self) -> None:
        with copied_cleanup_workspace() as workspace_one:
            with copied_cleanup_workspace() as workspace_two:
                artifact_root = workspace_one / "artifact-root"
                artifact_root.mkdir()

                env_one = run_exec_harness_env(
                    workspace_one,
                    CODEX_EXEC_HARNESS_NO_MKDIR="1",
                    CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                )
                env_two = run_exec_harness_env(
                    workspace_two,
                    CODEX_EXEC_HARNESS_NO_MKDIR="1",
                    CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                )

                self.assertEqual(0, env_one.returncode, env_one.stderr)
                self.assertEqual(0, env_two.returncode, env_two.stderr)
                self.assertNotEqual(env_one.stdout, env_two.stdout)
                self.assertNotIn("/local/codex-lab/", env_one.stdout)
                self.assertNotIn("/local/codex-lab/", env_two.stdout)

def run_local_cleanup(
    workspace: Path, *args: str, **env_overrides: str
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.pop("CODEX_EXEC_HARNESS_OUTPUT_ROOT", None)
    env.pop("CODEX_EXEC_HARNESS_REPORT_JSON", None)
    env.pop("CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT", None)
    env["CODEX_EXEC_HARNESS_CARGO_TARGET_DIR"] = str(
        workspace / ".tmp" / "exec-harness-target"
    )
    env["CODEX_LAB_CARGO_TARGET_DIR"] = str(
        workspace / ".tmp" / "local-cargo-target"
    )
    env.update(env_overrides)
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
    env.pop("CODEX_LAB_CARGO_TARGET_KEY", None)
    env.pop("CODEX_LAB_CARGO_TARGET_SCOPE", None)
    env.pop("CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT", None)
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
    env.pop("CODEX_EXEC_HARNESS_OUTPUT_ROOT", None)
    env.pop("CODEX_EXEC_HARNESS_REPORT_JSON", None)
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


def exported_value(stdout: str, key: str) -> str:
    prefix = f"export {key}="
    for line in stdout.splitlines():
        if line.startswith(prefix):
            return shlex.split(line)[1].split("=", 1)[1]
    raise AssertionError(f"missing export for {key}: {stdout}")


class copied_cleanup_workspace:
    def __enter__(self) -> Path:
        self._temp_dir = tempfile.TemporaryDirectory()
        workspace = Path(self._temp_dir.name)

        copy_file("justfile", workspace)
        copy_file("scripts/just-shell.py", workspace)
        copy_file("scripts/local/cleanup-space.sh", workspace)
        copy_file("scripts/local/cargo-build-env.sh", workspace)
        copy_file("scripts/local/exec-harness-env.sh", workspace)

        make_probe_dir(workspace / "codex-rs" / "target")
        make_probe_dir(workspace / ".tmp" / "codex-exec-harness")
        make_probe_dir(workspace / ".tmp" / "codex-exec-harness-ci")
        make_probe_dir(workspace / ".tmp" / "exec-harness-target")
        make_probe_dir(workspace / ".tmp" / "local-cargo-target")

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
