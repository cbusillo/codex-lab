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
            bin_dir = write_fake_rustc(Path(tmp), "aarch64-apple-darwin")

            completed = run_helper(
                env_file,
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                CARGO_TARGET_DIR=str(existing_target),
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(Path(tmp) / "artifact-root"),
            )

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn(
                "Using preconfigured Cargo target directory", completed.stdout
            )
            self.assertEqual(
                f"CARGO_TARGET_DIR={existing_target}\n"
                f"CODEX_LAB_BIN={existing_target / 'release' / 'codex-lab'}\n"
                "CODEX_LAB_CARGO_HOST=aarch64-apple-darwin\n"
                "CODEX_LAB_CARGO_PROFILE=release\n"
                "CODEX_LAB_CARGO_CACHE_MODE=preconfigured\n",
                env_file.read_text(),
            )

    def test_uses_configured_artifact_root_on_self_hosted_runner(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"
            summary_file = Path(tmp) / "summary"
            bin_dir = write_fake_rustc(Path(tmp), "x86_64-unknown-linux-gnu")
            artifact_root = Path(tmp) / "artifact-root"
            artifact_root.mkdir()

            completed = run_helper(
                env_file,
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                RUNNER_ENVIRONMENT="self-hosted",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                GITHUB_REPOSITORY="owner/repo",
                GITHUB_STEP_SUMMARY=str(summary_file),
            )

            target_dir = (
                artifact_root
                / "github-actions"
                / "cache"
                / "owner/repo"
                / "codex-lab-app"
                / "cargo-target-x86_64-unknown-linux-gnu-release"
            )
            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Using configured persistent target cache", completed.stdout)
            self.assertTrue(target_dir.exists())
            self.assertEqual(
                f"CARGO_TARGET_DIR={target_dir}\n"
                f"CODEX_LAB_BIN={target_dir / 'release' / 'codex-lab'}\n"
                "CODEX_LAB_CARGO_HOST=x86_64-unknown-linux-gnu\n"
                "CODEX_LAB_CARGO_PROFILE=release\n"
                "CODEX_LAB_CARGO_CACHE_MODE=persistent\n",
                env_file.read_text(),
            )
            self.assertIn("x86_64-unknown-linux-gnu", summary_file.read_text())
            self.assertNotIn(str(artifact_root), completed.stdout)
            self.assertNotIn(str(artifact_root), summary_file.read_text())

    def test_uses_requested_cargo_profile(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"
            bin_dir = write_fake_rustc(Path(tmp), "aarch64-apple-darwin")
            artifact_root = Path(tmp) / "artifact-root"
            artifact_root.mkdir()

            completed = run_helper(
                env_file,
                script_args=("codex-lab-app", "codex-lab", "ci-app"),
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
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
                / "cargo-target-aarch64-apple-darwin-ci-app"
            )
            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertEqual(
                f"CARGO_TARGET_DIR={target_dir}\n"
                f"CODEX_LAB_BIN={target_dir / 'ci-app' / 'codex-lab'}\n"
                "CODEX_LAB_CARGO_HOST=aarch64-apple-darwin\n"
                "CODEX_LAB_CARGO_PROFILE=ci-app\n"
                "CODEX_LAB_CARGO_CACHE_MODE=persistent\n",
                env_file.read_text(),
            )

    def test_rejects_invalid_cargo_profile(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"

            completed = run_helper(
                env_file,
                script_args=("codex-lab-app", "codex-lab", "bad/profile"),
            )

            self.assertEqual(2, completed.returncode)
            self.assertIn("invalid Cargo profile", completed.stderr)
            self.assertFalse(env_file.exists())

    def test_falls_back_to_default_target_dir_without_artifact_root(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_file = Path(tmp) / "github-env"

            completed = run_helper(env_file, cwd=Path(tmp))
            target_dir = REPO_ROOT / "codex-rs" / "target"

            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertIn("Using default Cargo target directory", completed.stdout)
            self.assertEqual(
                f"CARGO_TARGET_DIR={target_dir}\n"
                f"CODEX_LAB_BIN={target_dir / 'release' / 'codex-lab'}\n"
                f"CODEX_LAB_CARGO_HOST={rustc_host()}\n"
                "CODEX_LAB_CARGO_PROFILE=release\n"
                "CODEX_LAB_CARGO_CACHE_MODE=default\n",
                env_file.read_text(),
            )


def run_helper(
    env_file: Path,
    cwd: Path | None = None,
    script_args: tuple[str, ...] = ("codex-lab-app",),
    **overrides: str,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.pop("CARGO_TARGET_DIR", None)
    env.pop("CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT", None)
    env.pop("RUNNER_ENVIRONMENT", None)
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
        cwd=cwd,
        env=env,
        stderr=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
    )


def write_fake_rustc(root: Path, host: str) -> Path:
    bin_dir = root / "bin"
    bin_dir.mkdir()
    rustc = bin_dir / "rustc"
    rustc.write_text(
        "#!/usr/bin/env bash\n"
        "cat <<'EOF'\n"
        "rustc 1.95.0 (fake)\n"
        f"host: {host}\n"
        "release: 1.95.0\n"
        "EOF\n"
    )
    rustc.chmod(0o755)
    return bin_dir


def rustc_host() -> str:
    completed = subprocess.run(
        ["rustc", "-vV"],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    for line in completed.stdout.splitlines():
        if line.startswith("host: "):
            return line.split(maxsplit=1)[1]
    return "unknown-host"


if __name__ == "__main__":
    unittest.main()
