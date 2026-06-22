import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from textwrap import dedent


REPO_ROOT = Path(__file__).resolve().parents[2]
INSTALLER = REPO_ROOT / "scripts" / "local" / "install-codex-lab-dev.sh"


class InstallCodexLabDevTest(unittest.TestCase):
    def test_installs_managed_launcher_with_lab_home_default(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            bin_dir = root / "bin"
            lab_home = root / "home"

            result = subprocess.run(
                [
                    str(INSTALLER),
                    "--bin-dir",
                    str(bin_dir),
                    "--codex-lab-home",
                    str(lab_home),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=True,
            )

            launcher = bin_dir / "codex-lab"
            self.assertTrue(launcher.is_file())
            self.assertTrue(launcher.stat().st_mode & 0o111)
            contents = launcher.read_text(encoding="utf-8")
            self.assertIn("codex-lab-dev-shim", contents)
            self.assertIn("CODEX_LAB_HOME", contents)
            self.assertNotIn("CODEX_HOME", contents)
            self.assertIn("--bin codex-lab", contents)
            self.assertIn(str(REPO_ROOT), contents)
            self.assertIn(str(lab_home), contents)
            self.assertIn("scripts/local/cargo-build-env.sh", contents)
            self.assertIn("cargo_env=", contents)
            self.assertIn("eval", contents)
            self.assertIn("Installed Codex Lab dev launcher", result.stdout)

    def test_launcher_routes_build_through_cargo_env_helper(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            bin_dir = root / "bin"
            lab_home = root / "home"
            artifact_root = root / "artifacts"
            fake_bin = root / "fake-bin"
            artifact_root.mkdir()
            fake_bin.mkdir()
            fake_cargo = fake_bin / "cargo"
            fake_cargo.write_text(
                dedent(
                    """\
                    #!/bin/sh
                    set -eu
                    mkdir -p "$CARGO_TARGET_DIR/debug"
                    printf '#!/bin/sh\nprintf '\''target=%%s\\n'\'' "$CARGO_TARGET_DIR"\n' >"$CARGO_TARGET_DIR/debug/codex-lab"
                    chmod +x "$CARGO_TARGET_DIR/debug/codex-lab"
                    """
                ),
                encoding="utf-8",
            )
            fake_cargo.chmod(0o755)

            subprocess.run(
                [
                    str(INSTALLER),
                    "--bin-dir",
                    str(bin_dir),
                    "--codex-lab-home",
                    str(lab_home),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=True,
            )

            launcher = bin_dir / "codex-lab"
            result = subprocess.run(
                [str(launcher)],
                env={
                    "CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT": str(artifact_root),
                    "HOME": os.environ["HOME"],
                    "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
                },
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )

            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertIn(str(artifact_root / "local" / "codex-lab"), result.stdout)

    def test_launcher_fails_when_cargo_env_helper_fails(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            bin_dir = root / "bin"
            lab_home = root / "home"

            subprocess.run(
                [
                    str(INSTALLER),
                    "--bin-dir",
                    str(bin_dir),
                    "--codex-lab-home",
                    str(lab_home),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=True,
            )

            launcher = bin_dir / "codex-lab"
            result = subprocess.run(
                [str(launcher)],
                env={
                    "CODEX_LAB_CARGO_TARGET_DIR": "/dev/null/target",
                    "HOME": os.environ["HOME"],
                    "PATH": os.environ["PATH"],
                },
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("cargo: not found", result.stdout)

    def test_refuses_to_replace_unmanaged_launcher_without_force(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            launcher = bin_dir / "codex-lab"
            launcher.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")

            result = subprocess.run(
                [str(INSTALLER), "--bin-dir", str(bin_dir)],
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("refusing to replace non-managed launcher", result.stdout)

    def test_reports_missing_option_value(self) -> None:
        result = subprocess.run(
            [str(INSTALLER), "--bin-dir"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )

        self.assertEqual(result.returncode, 2)
        self.assertIn("--bin-dir requires a directory", result.stdout)


if __name__ == "__main__":
    unittest.main()
