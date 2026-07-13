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
            self.assertIn("scripts/local/codex_lab_provenance.py", contents)
            self.assertIn("CODEX_LAB_HOME/working", contents)
            self.assertIn("CODEX_LAB_CARGO_TARGET_SCOPE", contents)
            self.assertIn("PYTHON_BIN", contents)
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
                    cat >"$CARGO_TARGET_DIR/debug/codex-lab" <<'EOF'
                    #!/usr/bin/env python3
                    import json
                    import os
                    import sys
                    from pathlib import Path

                    executable = str(Path(__file__).resolve())
                    if sys.argv[1:] == ["debug", "provenance", "--json"]:
                        print(json.dumps({
                            "schema_version": 1,
                            "version": "test",
                            "source_commit": os.environ["FAKE_SOURCE_COMMIT"],
                            "dirty_state": os.environ["FAKE_DIRTY_STATE"],
                            "build_profile": "debug",
                            "build_channel": "dev",
                            "executable_path": executable,
                        }))
                    else:
                        print(f"candidate={executable}")
                    EOF
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
            source_commit = subprocess.check_output(
                ["git", "-C", str(REPO_ROOT), "rev-parse", "HEAD"], text=True
            ).strip()
            dirty_state = (
                "dirty"
                if subprocess.check_output(
                    [
                        "git",
                        "-C",
                        str(REPO_ROOT),
                        "status",
                        "--porcelain",
                        "--untracked-files=no",
                    ],
                    text=True,
                ).strip()
                else "clean"
            )
            launcher_env = {
                "CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT": str(artifact_root),
                "FAKE_DIRTY_STATE": dirty_state,
                "FAKE_SOURCE_COMMIT": source_commit,
                "HOME": os.environ["HOME"],
                "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
            }
            result = subprocess.run(
                [str(launcher)],
                env=launcher_env,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )

            self.assertEqual(result.returncode, 0, result.stdout)
            candidate_root = lab_home / "working" / "dogfood"
            self.assertIn(f"candidate={candidate_root.resolve()}", result.stdout)
            self.assertNotIn("cargo-target", result.stdout)

            stale = subprocess.run(
                [str(launcher)],
                env={**launcher_env, "FAKE_SOURCE_COMMIT": "0" * 40},
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("provenance stale", stale.stderr)

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

    def test_requires_supported_python(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            fake_bin = Path(temp_dir_name) / "bin"
            fake_bin.mkdir()
            fake_python = fake_bin / "python3"
            fake_python.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
            fake_python.chmod(0o755)

            result = subprocess.run(
                [str(INSTALLER), "--bin-dir", str(Path(temp_dir_name) / "output")],
                env={**os.environ, "PATH": f"{fake_bin}{os.pathsep}/usr/bin:/bin"},
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Python 3.10 or newer is required", result.stdout)

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
