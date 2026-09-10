import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("build-codex-package-archive.sh")


class BuildCodexPackageArchiveContractTests(unittest.TestCase):
    @staticmethod
    def wrapper_command(input_dir: Path, archive_dir: Path) -> list[str]:
        return [
            "bash",
            str(SCRIPT),
            "--target",
            "x86_64-unknown-linux-musl",
            "--bundle",
            "primary",
            "--entrypoint-dir",
            str(input_dir),
            "--archive-dir",
            str(archive_dir),
            "--rg-bin",
            str(input_dir / "rg"),
            "--bwrap-bin",
            str(input_dir / "bwrap"),
        ]

    @staticmethod
    def gate_command(root: Path) -> list[str]:
        return BuildCodexPackageArchiveContractTests.wrapper_command(
            root / "missing-inputs", root / "archives"
        )

    @staticmethod
    def gate_environment(root: Path) -> dict[str, str]:
        repo_root = Path(__file__).parents[2]
        environment = os.environ.copy()
        environment.update(
            {
                "CODEX_REPO_ROOT": str(repo_root),
                "CODEX_TARGET_OWNERSHIP": "1",
                "GITHUB_ACTIONS": "true",
                "GITHUB_WORKSPACE": str(repo_root),
                "RUNNER_TEMP": str(root / "runner-temp"),
            }
        )
        return environment

    def test_opt_in_requires_github_actions(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            environment = self.gate_environment(root)
            environment.pop("GITHUB_ACTIONS")

            result = subprocess.run(
                self.gate_command(root),
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("requires GitHub Actions", result.stderr)

    def test_opt_in_requires_absolute_runner_temp(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            environment = self.gate_environment(root)
            environment["RUNNER_TEMP"] = "relative-runner-temp"

            result = subprocess.run(
                self.gate_command(root),
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("absolute RUNNER_TEMP", result.stderr)

    def test_opt_in_rejects_unsupported_platform(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            tool_dir = root / "tools"
            tool_dir.mkdir()
            uname = tool_dir / "uname"
            uname.write_text("#!/bin/sh\nprintf '%s\\n' Darwin\n", encoding="utf-8")
            uname.chmod(uname.stat().st_mode | stat.S_IXUSR)
            environment = self.gate_environment(root)
            environment["PATH"] = f"{tool_dir}{os.pathsep}{os.environ['PATH']}"

            result = subprocess.run(
                self.gate_command(root),
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("enabled only on Linux", result.stderr)

    def test_owned_wrapper_preserves_stale_target_on_collision(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            runner_temp = root / "runner-temp"
            input_dir = root / "inputs"
            archive_dir = root / "archives"
            tool_dir = root / "tools"
            runner_temp.mkdir()
            input_dir.mkdir()
            tool_dir.mkdir()
            uname = tool_dir / "uname"
            uname.write_text("#!/bin/sh\nprintf '%s\\n' Linux\n", encoding="utf-8")
            uname.chmod(uname.stat().st_mode | stat.S_IXUSR)
            for name in ("codex", "codex-code-mode-host", "rg", "bwrap"):
                path = input_dir / name
                path.write_text("fixture", encoding="utf-8")
                path.chmod(path.stat().st_mode | stat.S_IXUSR)

            repo_root = Path(__file__).parents[2]
            archive_dir.mkdir()
            gzip_archive = (
                archive_dir / "codex-package-x86_64-unknown-linux-musl.tar.gz"
            )
            gzip_archive.write_bytes(b"archive-sentinel")
            environment = os.environ.copy()
            environment.update(
                {
                    "CODEX_REPO_ROOT": str(repo_root),
                    "CODEX_TARGET_OWNERSHIP": "1",
                    "GITHUB_ACTIONS": "true",
                    "GITHUB_WORKSPACE": str(repo_root),
                    "PATH": f"{tool_dir}{os.pathsep}{os.environ['PATH']}",
                    "RUNNER_TEMP": str(runner_temp),
                }
            )
            command = self.wrapper_command(input_dir, archive_dir)
            first = subprocess.run(
                command, env=environment, capture_output=True, text=True, check=False
            )
            self.assertEqual(0, first.returncode, first.stderr)
            with tarfile.open(gzip_archive, "r:gz") as archive:
                members = archive.getnames()
                self.assertIn("codex-package.json", members)
                self.assertFalse(
                    any(
                        member == ".codex-target-ownership"
                        or member.startswith(".codex-target-ownership/")
                        for member in members
                    )
                )
            self.assertNotEqual(gzip_archive.read_bytes(), b"archive-sentinel")

            package_dir = runner_temp / "codex-package-x86_64-unknown-linux-musl"
            sentinel = package_dir / "codex-package.json"
            original = sentinel.read_bytes()
            inspect = subprocess.run(
                [
                    sys.executable,
                    str(repo_root / "scripts/local/target_ownership.py"),
                    "inspect",
                    "--root",
                    str(runner_temp),
                    "--target",
                    package_dir.name,
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(21, inspect.returncode, inspect.stderr)
            self.assertEqual(
                "released-unverified", json.loads(inspect.stdout)["status"]
            )
            second = subprocess.run(
                command, env=environment, capture_output=True, text=True, check=False
            )
            self.assertNotEqual(second.returncode, 0)
            self.assertIn("target-already-claimed", second.stderr)
            self.assertEqual(sentinel.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
