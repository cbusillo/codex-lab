#!/usr/bin/env python3
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "github" / "configure-codex-lab-cargo-cache.sh"


class ConfigureCodexLabCargoCacheTest(unittest.TestCase):
    def test_uses_existing_cargo_target_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"
            existing_target = Path(tmp) / "existing-target"

            completed = run_helper(
                env_file,
                CARGO_TARGET_DIR=str(existing_target),
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(Path(tmp) / "artifact-root"),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn(
                "Using preconfigured Cargo target directory", completed.stdout
            )
            self.assertEqual(
                f"CARGO_TARGET_DIR={existing_target}\n"
                f"CODEX_LAB_BIN={existing_target / 'release' / 'codex-lab'}\n",
                env_file.read_text(),
            )

    def test_uses_configured_artifact_root_on_self_hosted_runner(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"
            artifact_root = Path(tmp) / "artifact-root"
            artifact_root.mkdir()

            completed = run_helper(
                env_file,
                RUNNER_ENVIRONMENT="self-hosted",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                GITHUB_REPOSITORY="owner/repo",
            )

            target_dir = (
                artifact_root
                / "github-actions"
                / "cache"
                / "owner/repo"
                / "codex-lab-app"
                / "cargo-target-aarch64-apple-darwin-release"
            )
            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Using configured persistent target cache", completed.stdout)
            self.assertTrue(target_dir.exists())
            self.assertEqual(
                f"CARGO_TARGET_DIR={target_dir}\n"
                f"CODEX_LAB_BIN={target_dir / 'release' / 'codex-lab'}\n",
                env_file.read_text(),
            )

    def test_falls_back_to_default_target_dir_without_artifact_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"

            completed = run_helper(env_file, cwd=Path(tmp))
            target_dir = REPO_ROOT / "codex-rs" / "target"

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Using default Cargo target directory", completed.stdout)
            self.assertEqual(
                f"CARGO_TARGET_DIR={target_dir}\n"
                f"CODEX_LAB_BIN={target_dir / 'release' / 'codex-lab'}\n",
                env_file.read_text(),
            )


def run_helper(
    env_file: Path, cwd: Path | None = None, **overrides: str
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.pop("CARGO_TARGET_DIR", None)
    env.pop("CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT", None)
    env.update(
        {
            "GITHUB_ENV": str(env_file),
            "GITHUB_REPOSITORY": "cbusillo/codex-lab",
        }
    )
    env.update(overrides)
    return subprocess.run(
        [str(SCRIPT), "codex-lab-app"],
        check=False,
        cwd=cwd,
        env=env,
        stderr=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )


if __name__ == "__main__":
    unittest.main()
