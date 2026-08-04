import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from textwrap import dedent


REPO_ROOT = Path(__file__).resolve().parents[2]
INSTALLER = REPO_ROOT / "scripts" / "local" / "install-codex-lab-dev.sh"


class InstallCodexLabDevTest(unittest.TestCase):
    def create_fake_checkout(self, root: Path) -> tuple[Path, Path, dict[str, str]]:
        checkout = root / "checkout with spaces"
        for relative_path in (
            "scripts/local/cargo-build-env.sh",
            "scripts/local/codex_lab_provenance.py",
            "scripts/local/install-codex-lab-dev.sh",
        ):
            destination = checkout / relative_path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(REPO_ROOT / relative_path, destination)
        (checkout / "codex-rs").mkdir()
        subprocess.run(["git", "init", "-q"], cwd=checkout, check=True)
        subprocess.run(
            ["git", "config", "user.name", "Codex Lab Test"],
            cwd=checkout,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.email", "codex-lab@example.invalid"],
            cwd=checkout,
            check=True,
        )
        subprocess.run(["git", "add", "."], cwd=checkout, check=True)
        subprocess.run(
            ["git", "commit", "-q", "-m", "test"], cwd=checkout, check=True
        )
        source_commit = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=checkout, text=True
        ).strip()

        fake_bin = root / "fake bin"
        fake_bin.mkdir()
        (fake_bin / "bash").symlink_to("/bin/bash")
        fake_cargo = fake_bin / "cargo"
        fake_cargo.write_text(
            dedent(
                """\
                #!/bin/sh
                set -eu
                printf 'build\\n' >>"$FAKE_CARGO_LOG"
                printf '%s\\n' "$PWD" >"$FAKE_CARGO_PWD"
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
                        "source_commit": os.environ["FAKE_BINARY_COMMIT"],
                        "dirty_state": os.environ["FAKE_DIRTY_STATE"],
                        "build_profile": "debug",
                        "build_channel": "dev",
                        "executable_path": executable,
                    }))
                else:
                    print(f"candidate={executable}")
                EOF
                chmod +x "$CARGO_TARGET_DIR/debug/codex-lab"
                cat >"$CARGO_TARGET_DIR/debug/codex-code-mode-host" <<'EOF'
                #!/bin/sh
                exit 0
                EOF
                chmod +x "$CARGO_TARGET_DIR/debug/codex-code-mode-host"
                """
            ),
            encoding="utf-8",
        )
        fake_cargo.chmod(0o755)
        process_home = root / "process home"
        process_home.mkdir()
        environment = {
            **os.environ,
            "CODEX_LAB_CARGO_TARGET_DIR": str(root / "target with spaces"),
            "FAKE_BINARY_COMMIT": source_commit,
            "FAKE_CARGO_LOG": str(root / "cargo.log"),
            "FAKE_CARGO_PWD": str(root / "cargo.pwd"),
            "FAKE_DIRTY_STATE": "clean",
            "HOME": str(process_home),
            "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
        }
        return checkout, fake_cargo, environment

    def install_fake_candidate(
        self, root: Path
    ) -> tuple[Path, Path, Path, dict[str, str], subprocess.CompletedProcess[str]]:
        checkout, fake_cargo, environment = self.create_fake_checkout(root)
        bin_dir = root / "bin with spaces"
        lab_home = root / "home with spaces"
        caller_dir = root / "caller with spaces"
        caller_dir.mkdir()
        result = subprocess.run(
            [
                str(checkout / "scripts/local/install-codex-lab-dev.sh"),
                "--bin-dir",
                str(bin_dir),
                "--codex-lab-home",
                str(lab_home),
            ],
            cwd=caller_dir,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout)
        return checkout, fake_cargo, lab_home, environment, result

    def test_installs_launcher_pinned_to_staged_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, fake_cargo, lab_home, environment, install = (
                self.install_fake_candidate(root)
            )
            launcher = root / "bin with spaces" / "codex-lab"

            self.assertTrue(launcher.is_file())
            self.assertTrue(launcher.stat().st_mode & 0o111)
            contents = launcher.read_text(encoding="utf-8")
            self.assertIn("codex-lab-dev-shim", contents)
            self.assertIn("CODEX_LAB_HOME", contents)
            self.assertNotIn("CODEX_HOME", contents)
            self.assertIn(str(lab_home), contents)
            self.assertIn(str((lab_home / "working" / "dogfood").resolve()), contents)
            self.assertNotIn(str(checkout), contents)
            self.assertNotIn("cargo", contents)
            self.assertNotIn("python", contents.lower())
            self.assertIn("Installed Codex Lab dev launcher", install.stdout)
            self.assertIn("Pinned dogfood candidate", install.stdout)
            self.assertEqual((root / "cargo.log").read_text(encoding="utf-8"), "build\n")
            self.assertEqual(
                Path((root / "cargo.pwd").read_text(encoding="utf-8").strip()).resolve(),
                (checkout / "codex-rs").resolve(),
            )

            fake_cargo.unlink()
            shutil.rmtree(checkout)
            runtime_home = root / "runtime-home"
            caller_dir = root / "runtime caller"
            caller_dir.mkdir()
            launch = subprocess.run(
                [str(launcher)],
                cwd=caller_dir,
                env={
                    "CODEX_LAB_HOME": str(runtime_home),
                    "HOME": environment["HOME"],
                    "PATH": os.environ["PATH"],
                },
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(launch.returncode, 0, launch.stderr)
            self.assertIn("candidate=", launch.stdout)
            self.assertTrue(runtime_home.is_dir())
            self.assertEqual((root / "cargo.log").read_text(encoding="utf-8"), "build\n")
            candidate = Path(launch.stdout.split("candidate=", maxsplit=1)[1].strip())
            companion = candidate.parent / "codex-code-mode-host"
            self.assertTrue(companion.is_file())
            self.assertFalse(companion.stat().st_mode & 0o222)

    def test_failed_reinstall_preserves_existing_launcher(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            checkout, _, lab_home, environment, _ = self.install_fake_candidate(root)
            launcher = root / "bin with spaces" / "codex-lab"
            original_contents = launcher.read_text(encoding="utf-8")

            failed = subprocess.run(
                [
                    str(checkout / "scripts/local/install-codex-lab-dev.sh"),
                    "--bin-dir",
                    str(launcher.parent),
                    "--codex-lab-home",
                    str(lab_home),
                ],
                env={**environment, "FAKE_BINARY_COMMIT": "0" * 40},
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("provenance stale", failed.stderr)
            self.assertEqual(launcher.read_text(encoding="utf-8"), original_contents)

    def test_refuses_to_replace_unmanaged_launcher_without_force(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir_name:
            root = Path(temp_dir_name)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            launcher = bin_dir / "codex-lab"
            launcher.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")

            result = subprocess.run(
                [
                    str(INSTALLER),
                    "--bin-dir",
                    str(bin_dir),
                    "--codex-lab-home",
                    str(root / "home"),
                ],
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
                env={
                    **os.environ,
                    "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
                },
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
