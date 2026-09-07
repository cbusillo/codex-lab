import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("cargo-build-env.sh")


def base_environment() -> dict[str, str]:
    environment = os.environ.copy()
    for name in (
        "CARGO_TARGET_DIR",
        "CODEX_LAB_CARGO_TARGET_DIR",
        "CODEX_LAB_CARGO_TARGET_KEY",
        "CODEX_LAB_CARGO_TARGET_NO_MKDIR",
        "CODEX_LAB_CARGO_TARGET_SCOPE",
        "CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT",
        "CODEX_LAB_DEVELOPER_ARTIFACTS_VOLUME_UUID",
    ):
        environment.pop(name, None)
    return environment


class CargoBuildEnvTest(unittest.TestCase):
    def run_script(self, **updates: str) -> subprocess.CompletedProcess[str]:
        environment = base_environment()
        environment.update(updates)
        return subprocess.run(
            [SCRIPT],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    @staticmethod
    def create_volume_tool_stubs(
        bin_dir: Path, *, mount_point: Path, volume_uuid: str = "ACTUAL-UUID"
    ) -> None:
        uname = bin_dir / "uname"
        uname.write_text(
            "#!/bin/sh\n"
            'case "$1" in\n'
            "-s) printf 'Darwin\\n' ;;\n"
            "-m) printf 'arm64\\n' ;;\n"
            "*) printf 'Darwin\\n' ;;\n"
            "esac\n"
        )
        uname.chmod(uname.stat().st_mode | stat.S_IXUSR)
        diskutil = bin_dir / "diskutil"
        diskutil.write_text("#!/bin/sh\nprintf 'plist fixture\\n'\n")
        diskutil.chmod(diskutil.stat().st_mode | stat.S_IXUSR)
        plutil = bin_dir / "plutil"
        plutil.write_text(
            "#!/bin/sh\n"
            "cat >/dev/null\n"
            'case "$2" in\n'
            f"MountPoint) printf '%s\\n' {str(mount_point)!r} ;;\n"
            f"VolumeUUID) printf '%s\\n' {volume_uuid!r} ;;\n"
            "*) exit 1 ;;\n"
            "esac\n"
        )
        plutil.chmod(plutil.stat().st_mode | stat.S_IXUSR)

    def test_unconfigured_root_uses_portable_repository_target(self) -> None:
        completed = self.run_script(CODEX_LAB_CARGO_TARGET_NO_MKDIR="1")

        self.assertEqual(completed.returncode, 0)
        self.assertEqual(
            completed.stdout.strip(), str(SCRIPT.parents[2] / "codex-rs/target")
        )
        self.assertIn("portable repository target", completed.stderr)

    def test_missing_configured_root_fails_without_creating_it(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            artifact_root = Path(temporary_directory) / "missing-volume"
            completed = self.run_script(
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root)
            )

            self.assertEqual(completed.returncode, 1)
            self.assertEqual(completed.stdout, "")
            self.assertIn("configured artifact root is missing", completed.stderr)
            self.assertFalse(artifact_root.exists())

    def test_explicit_target_preserves_unmanaged_override(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            target = Path(temporary_directory) / "explicit target"
            completed = self.run_script(
                CODEX_LAB_CARGO_TARGET_DIR=str(target),
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(
                    Path(temporary_directory) / "missing-volume"
                ),
            )

        self.assertEqual(completed.returncode, 0)
        self.assertEqual(completed.stdout.strip(), str(target))
        self.assertIn("artifact root unmanaged", completed.stderr)

    @unittest.skipIf(
        hasattr(os, "geteuid") and os.geteuid() == 0,
        "root bypasses permission-bit writability checks",
    )
    def test_unwritable_configured_root_fails(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            artifact_root = Path(temporary_directory) / "read-only-volume"
            artifact_root.mkdir()
            artifact_root.chmod(stat.S_IRUSR | stat.S_IXUSR)
            try:
                completed = self.run_script(
                    CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root)
                )
            finally:
                artifact_root.chmod(stat.S_IRWXU)

        self.assertEqual(completed.returncode, 1)
        self.assertEqual(completed.stdout, "")
        self.assertIn("configured artifact root is not writable", completed.stderr)

    def test_wrong_configured_volume_uuid_fails(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            self.create_volume_tool_stubs(bin_dir, mount_point=root.resolve())

            completed = self.run_script(
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(root),
                CODEX_LAB_DEVELOPER_ARTIFACTS_VOLUME_UUID="EXPECTED-UUID",
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            )

            self.assertFalse((root / "local").exists())

        self.assertEqual(completed.returncode, 1)
        self.assertEqual(completed.stdout, "")
        self.assertIn(
            "on volume UUID ACTUAL-UUID; expected EXPECTED-UUID", completed.stderr
        )

    def test_matching_configured_volume_uuid_uses_managed_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            self.create_volume_tool_stubs(
                bin_dir, mount_point=root.resolve(), volume_uuid="expected-uuid"
            )

            completed = self.run_script(
                CODEX_LAB_CARGO_TARGET_KEY="test-worktree",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(root),
                CODEX_LAB_DEVELOPER_ARTIFACTS_VOLUME_UUID="EXPECTED-UUID",
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            )

        self.assertEqual(completed.returncode, 0)
        self.assertIn(
            str(
                root.resolve() / "local/codex-lab/worktrees/test-worktree/cargo-target"
            ),
            completed.stdout,
        )
        self.assertIn("using managed artifact root", completed.stderr)

    def test_verified_volume_without_expected_uuid_uses_managed_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            self.create_volume_tool_stubs(bin_dir, mount_point=root.resolve())

            completed = self.run_script(
                CODEX_LAB_CARGO_TARGET_KEY="test-worktree",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(root),
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            )

        self.assertEqual(completed.returncode, 0)
        self.assertIn("using verified artifact volume", completed.stderr)

    def test_non_macos_configured_directory_remains_portable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            uname = bin_dir / "uname"
            uname.write_text("#!/bin/sh\nprintf 'Linux\\n'\n")
            uname.chmod(uname.stat().st_mode | stat.S_IXUSR)

            completed = self.run_script(
                CODEX_LAB_CARGO_TARGET_KEY="test-worktree",
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(root),
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            )

        self.assertEqual(completed.returncode, 0)
        self.assertIn("managed artifact path", completed.stderr)
        self.assertIn("volume identity not verified outside macOS", completed.stderr)

    def test_non_macos_expected_volume_uuid_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            uname = bin_dir / "uname"
            uname.write_text("#!/bin/sh\nprintf 'Linux\\n'\n")
            uname.chmod(uname.stat().st_mode | stat.S_IXUSR)

            completed = self.run_script(
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(root),
                CODEX_LAB_DEVELOPER_ARTIFACTS_VOLUME_UUID="EXPECTED-UUID",
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            )

            self.assertFalse((root / "local").exists())

        self.assertEqual(completed.returncode, 1)
        self.assertIn("cannot verify configured artifact volume UUID", completed.stderr)

    def test_ordinary_directory_on_root_volume_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            artifact_root = root / "ordinary-directory"
            artifact_root.mkdir()
            bin_dir = root / "bin"
            bin_dir.mkdir()
            self.create_volume_tool_stubs(bin_dir, mount_point=Path("/"))

            completed = self.run_script(
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            )

            self.assertFalse((artifact_root / "local").exists())

        self.assertEqual(completed.returncode, 1)
        self.assertIn("but its mounted volume root is /", completed.stderr)

    def test_wrong_mounted_volume_root_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            artifact_root = root / "configured-volume"
            artifact_root.mkdir()
            other_volume = root / "other-volume"
            other_volume.mkdir()
            bin_dir = root / "bin"
            bin_dir.mkdir()
            self.create_volume_tool_stubs(bin_dir, mount_point=other_volume.resolve())

            completed = self.run_script(
                CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT=str(artifact_root),
                PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            )

            self.assertFalse((artifact_root / "local").exists())

        self.assertEqual(completed.returncode, 1)
        self.assertIn(str(other_volume.resolve()), completed.stderr)


if __name__ == "__main__":
    unittest.main()
