#!/usr/bin/env python3

from pathlib import Path
import hashlib
import os
import plistlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.layout import CodexLabAppOptions
from codex_lab_package.layout import _launcher_script
from codex_lab_package.layout import build_codex_lab_app
from codex_lab_package.smoke import smoke_check


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
                    codex_app_path=Path("/custom/OpenAI Desktop.app"),
                    shim_dir=root / "bin",
                    bundle_identifier="dev.example.codex-lab-test",
                    short_version="1.2.3",
                    bundle_version="42",
                    source_commit="a" * 40,
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
                    "CFBundleIconFile": "CodexLab.icns",
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
            icon = root / "Codex Lab.app/Contents/Resources/CodexLab.icns"
            self.assertEqual(icon.read_bytes()[:4], b"icns")
            for expected in (
                "EXPECTED_SOURCE_COMMIT='" + "a" * 40 + "'",
                "com.openai.codex",
                "2DC432GLL2",
            ):
                self.assertIn(expected, launcher)
            candidate_block = launcher.split('candidate_apps="', maxsplit=1)[1].split(
                '"\n\nif', maxsplit=1
            )[0]
            self.assertEqual(
                candidate_block.splitlines(),
                [
                    "$CONFIGURED_CODEX_APP",
                    "/Applications/ChatGPT.app",
                    "$home_chatgpt_app",
                    "/Applications/Codex.app",
                    "$home_codex_app",
                ],
            )

            shim = result.shim_path.read_text(encoding="utf-8")
            self.assertNotIn(str(result.embedded_cli_path), shim)
            self.assertIn("../Codex Lab.app", shim)
            self.assertIn(str(result.app_dir), shim)
            self.assertIn("CODEX_LAB_APP_PATH", shim)
            self.assertIn("/Applications/Codex Lab.app", shim)
            self.assertIn('exec "$LAB_CLI" "$@"', shim)

            smoke_check(result.app_dir, result.shim_path)

            completed = subprocess.run(
                [str(result.shim_path), "-c", "printf shim-ok"],
                check=True,
                env={**os.environ, "CODEX_LAB_APP_PATH": str(result.app_dir)},
                stdout=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.stdout, "shim-ok")

            override_cli = root / "Override Codex Lab.app/Contents/Resources/codex-lab"
            override_cli.parent.mkdir(parents=True)
            override_cli.write_text("#!/bin/sh\nprintf override-ok\n", encoding="utf-8")
            os.chmod(override_cli, 0o755)
            completed = subprocess.run(
                [str(result.shim_path), "-c", "printf sibling-was-used"],
                check=True,
                env={**os.environ, "CODEX_LAB_APP_PATH": str(override_cli.parents[2])},
                stdout=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.stdout, "override-ok")

            completed = subprocess.run(
                [str(result.shim_path), "-c", "printf sibling-ok"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.stdout, "sibling-ok")

    def test_launcher_executes_exact_cli_and_fails_closed_for_running_app(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            app_dir = root / "Codex Lab.app"
            embedded_cli = app_dir / "Contents/Resources/codex-lab"
            launcher = app_dir / "Contents/MacOS/Codex Lab Launcher"
            official_app = root / "Official ChatGPT.app"
            official_info = official_app / "Contents/Info.plist"
            official_info.parent.mkdir(parents=True)
            official_info.touch()
            embedded_cli.parent.mkdir(parents=True)
            launcher.parent.mkdir(parents=True)

            embedded_cli.write_text(
                """#!/bin/sh
set -eu
if [ "${1:-}" = debug ] && [ "${2:-}" = provenance ] && [ "${3:-}" = --json ]; then
  printf '{"schema_version":1,"version":"1.2.3","source_commit":"%s","dirty_state":"clean","build_profile":"release","build_channel":"lab","executable_path":"%s"}\\n' "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" "$0"
  exit 0
fi
case " $* " in
  *" app-server "*) printf '%s\\n' "$0" > "$CHILD_LOG" ;;
  *) exit 2 ;;
esac
""",
                encoding="utf-8",
            )
            os.chmod(embedded_cli, 0o755)

            fake_plutil = root / "plutil"
            fake_plutil.write_text(
                """#!/usr/bin/env python3
import json
import sys

key = sys.argv[2]
if key == "CFBundleIdentifier":
    print("com.openai.codex")
elif key == "CFBundleExecutable":
    print("ChatGPT")
else:
    with open(sys.argv[-1], encoding="utf-8") as handle:
        print(json.load(handle)[key])
""",
                encoding="utf-8",
            )
            fake_codesign = root / "codesign"
            fake_codesign.write_text(
                """#!/bin/sh
if [ "${1:-}" = --verify ]; then
  exit 0
fi
echo 'TeamIdentifier = 2DC432GLL2' >&2
""",
                encoding="utf-8",
            )
            fake_lsappinfo = root / "lsappinfo"
            fake_lsappinfo.write_text(
                """#!/bin/sh
if [ -n "${RUNNING_APP_PATH:-}" ]; then
  printf 'ASN:0x0-0x1-"ChatGPT": %s\\n' "$RUNNING_APP_PATH"
fi
""",
                encoding="utf-8",
            )
            fake_shasum = root / "shasum"
            fake_shasum.write_text(
                """#!/usr/bin/env python3
import hashlib
import sys

path = sys.argv[-1]
with open(path, "rb") as handle:
    print(hashlib.sha256(handle.read()).hexdigest(), path)
""",
                encoding="utf-8",
            )
            open_log = root / "open.log"
            child_log = root / "child.log"
            fake_open = root / "open"
            fake_open.write_text(
                """#!/bin/sh
export CODEX_CLI_PATH="${3#CODEX_CLI_PATH=}"
printf 'cli=%s\\nargs=%s\\n' "$CODEX_CLI_PATH" "$*" > "$OPEN_LOG"
"$CODEX_CLI_PATH" -c features.code_mode_host=true app-server
""",
                encoding="utf-8",
            )
            for executable in (
                fake_plutil,
                fake_codesign,
                fake_lsappinfo,
                fake_shasum,
                fake_open,
            ):
                os.chmod(executable, 0o755)

            launcher.write_text(
                _launcher_script(
                    embedded_cli_path=embedded_cli,
                    codex_app_path=official_app,
                    expected_cli_sha256=hashlib.sha256(
                        embedded_cli.read_bytes()
                    ).hexdigest(),
                    expected_source_commit="a" * 40,
                    expected_version="1.2.3",
                    codesign_path=fake_codesign,
                    lsappinfo_path=fake_lsappinfo,
                    open_path=fake_open,
                    plutil_path=fake_plutil,
                    shasum_path=fake_shasum,
                ),
                encoding="utf-8",
            )
            os.chmod(launcher, 0o755)
            environment = {
                **os.environ,
                "CHILD_LOG": str(child_log),
                "OPEN_LOG": str(open_log),
            }

            completed = subprocess.run(
                [str(launcher)],
                check=True,
                env=environment,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertIn(
                f"Selected OpenAI coding desktop app: {official_app}", completed.stderr
            )
            self.assertIn("commit=" + "a" * 40, completed.stderr)
            self.assertNotIn("executable_path", completed.stderr)
            self.assertEqual(
                child_log.read_text(encoding="utf-8").strip(), str(embedded_cli)
            )
            open_contents = open_log.read_text(encoding="utf-8")
            self.assertIn(f"cli={embedded_cli}", open_contents)
            self.assertIn(f"--env CODEX_CLI_PATH={embedded_cli}", open_contents)
            self.assertIn(str(official_app), open_contents)

            open_log.unlink()
            environment["RUNNING_APP_PATH"] = str(
                official_app / "Contents/MacOS/ChatGPT"
            )
            completed = subprocess.run(
                [str(launcher)],
                check=False,
                env=environment,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn("already running", completed.stderr)
            self.assertFalse(open_log.exists())

    def test_shim_falls_back_to_exact_build_app_path(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex_bin = root / "fake-codex"
            codex_bin.write_text("#!/bin/sh\nprintf built-app-ok\n", encoding="utf-8")
            os.chmod(codex_bin, 0o755)

            result = build_codex_lab_app(
                CodexLabAppOptions(
                    app_dir=root / "staging" / "Codex Lab.app",
                    codex_bin=codex_bin,
                    shim_dir=root / "bin",
                )
            )
            self.assertIsNotNone(result.shim_path)
            assert result.shim_path is not None

            completed = subprocess.run(
                [str(result.shim_path)],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.stdout, "built-app-ok")

    def test_build_script_infers_version_and_source_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex_bin = root / "fake-codex"
            codex_bin.write_text(
                """#!/bin/sh
if [ "${1:-}" = debug ] && [ "${2:-}" = provenance ] && [ "${3:-}" = --json ]; then
  printf '{"schema_version":1,"version":"1.2.3","source_commit":"%s","dirty_state":"clean","build_profile":"release","build_channel":"lab","executable_path":"%s"}\\n' "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" "$0"
  exit 0
fi
exit 2
""",
                encoding="utf-8",
            )
            os.chmod(codex_bin, 0o755)
            app_dir = root / "Codex Lab.app"
            script = Path(__file__).resolve().parents[1] / "build_codex_lab_app.py"

            completed = subprocess.run(
                [
                    sys.executable,
                    str(script),
                    "--codex-bin",
                    str(codex_bin),
                    "--app-dir",
                    str(app_dir),
                ],
                check=True,
                capture_output=True,
                text=True,
            )

            with (app_dir / "Contents/Info.plist").open("rb") as handle:
                info = plistlib.load(handle)
            self.assertEqual(info["CFBundleShortVersionString"], "1.2.3")
            launcher = (app_dir / "Contents/MacOS/Codex Lab Launcher").read_text(
                encoding="utf-8"
            )
            self.assertIn("EXPECTED_VERSION='1.2.3'", launcher)
            self.assertIn("EXPECTED_SOURCE_COMMIT='" + "a" * 40 + "'", launcher)
            self.assertIn("Built Codex Lab app bundle", completed.stdout)

    def test_build_script_rejects_metadata_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex_bin = root / "fake-codex"
            codex_bin.write_text(
                """#!/bin/sh
printf '{"schema_version":1,"version":"1.2.3","source_commit":"%s","dirty_state":"clean","build_profile":"release","build_channel":"lab","executable_path":"%s"}\\n' "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" "$0"
""",
                encoding="utf-8",
            )
            os.chmod(codex_bin, 0o755)
            script = Path(__file__).resolve().parents[1] / "build_codex_lab_app.py"

            completed = subprocess.run(
                [
                    sys.executable,
                    str(script),
                    "--codex-bin",
                    str(codex_bin),
                    "--app-dir",
                    str(root / "Codex Lab.app"),
                    "--short-version",
                    "9.9.9",
                ],
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(completed.returncode, 2)
            self.assertIn(
                "does not match the embedded CLI provenance", completed.stderr
            )

    def test_refuses_to_replace_existing_app_without_force(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            app_dir = root / "Codex Lab.app"
            app_dir.mkdir()

            with self.assertRaises(FileExistsError):
                build_codex_lab_app(
                    CodexLabAppOptions(app_dir=app_dir, codex_bin=Path("/bin/sh"))
                )

    def test_rejects_malformed_source_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            with self.assertRaisesRegex(ValueError, "40-character hex SHA"):
                build_codex_lab_app(
                    CodexLabAppOptions(
                        app_dir=Path(temp_dir) / "Codex Lab.app",
                        codex_bin=Path("/bin/sh"),
                        source_commit="not-a-commit",
                    )
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
