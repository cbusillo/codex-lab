#!/usr/bin/env python3

from pathlib import Path
import os
import plistlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.layout import CodexLabAppOptions
from codex_lab_package.layout import build_codex_lab_app


class BuildCodexLabAppTest(unittest.TestCase):
    def test_builds_launcher_bundle_with_embedded_cli_and_shim(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex_bin = root / "fake-codex"
            codex_bin.write_text('#!/bin/sh\nexec /bin/sh "$@"\n', encoding="utf-8")
            os.chmod(codex_bin, 0o755)

            result = build_codex_lab_app(
                CodexLabAppOptions(
                    app_dir=root / "Codex Lab.app",
                    codex_bin=codex_bin,
                    codex_app_path=Path("/Applications/Codex.app"),
                    shim_dir=root / "bin",
                    bundle_identifier="dev.example.codex-lab-test",
                    short_version="1.2.3",
                    bundle_version="42",
                )
            )

            self.assertEqual(result.app_dir, (root / "Codex Lab.app").resolve())
            self.assertEqual(
                result.embedded_cli_path,
                (root / "Codex Lab.app/Contents/Resources/codex-lab").resolve(),
            )
            self.assertEqual(
                result.launcher_path,
                (root / "Codex Lab.app/Contents/MacOS/Codex Lab Launcher").resolve(),
            )
            self.assertEqual(result.shim_path, root / "bin/codex-lab")
            self.assertIsNotNone(result.shim_path)
            assert result.shim_path is not None
            self.assertFalse(result.shim_path.is_symlink())

            info_plist = root / "Codex Lab.app/Contents/Info.plist"
            with info_plist.open("rb") as handle:
                info = plistlib.load(handle)
            self.assertEqual(
                info,
                {
                    "CFBundleDisplayName": "Codex Lab",
                    "CFBundleExecutable": "Codex Lab Launcher",
                    "CFBundleIdentifier": "dev.example.codex-lab-test",
                    "CFBundleName": "Codex Lab",
                    "CFBundlePackageType": "APPL",
                    "CFBundleShortVersionString": "1.2.3",
                    "CFBundleVersion": "42",
                    "LSMinimumSystemVersion": "13.0",
                    "NSHighResolutionCapable": True,
                },
            )

            launcher = result.launcher_path.read_text(encoding="utf-8")
            self.assertIn("CODEX_CLI_PATH", launcher)
            self.assertIn("APP_CONTENTS_DIR", launcher)
            self.assertIn("Resources/codex-lab", launcher)
            self.assertIn("open -n", launcher)
            self.assertIn("--env", launcher)

            shim = result.shim_path.read_text(encoding="utf-8")
            self.assertNotIn(str(result.embedded_cli_path), shim)
            self.assertIn("CODEX_LAB_APP_PATH", shim)
            self.assertIn("/Applications/Codex Lab.app", shim)
            self.assertIn('exec "$LAB_CLI" "$@"', shim)

            completed = subprocess.run(
                [str(result.shim_path), "-c", "printf shim-ok"],
                check=True,
                env={**os.environ, "CODEX_LAB_APP_PATH": str(result.app_dir)},
                stdout=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.stdout, "shim-ok")

    def test_refuses_to_replace_existing_app_without_force(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            app_dir = root / "Codex Lab.app"
            app_dir.mkdir()

            with self.assertRaises(FileExistsError):
                build_codex_lab_app(
                    CodexLabAppOptions(app_dir=app_dir, codex_bin=Path("/bin/sh"))
                )

    def test_force_replaces_existing_shim(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            shim_dir = root / "bin"
            shim_dir.mkdir()
            os.symlink(Path("/bin/sh"), shim_dir / "codex-lab")

            result = build_codex_lab_app(
                CodexLabAppOptions(
                    app_dir=root / "Codex Lab.app",
                    codex_bin=Path("/bin/sh"),
                    shim_dir=shim_dir,
                    force=True,
                )
            )

            self.assertIsNotNone(result.shim_path)
            assert result.shim_path is not None
            self.assertFalse(result.shim_path.is_symlink())


if __name__ == "__main__":
    unittest.main()
