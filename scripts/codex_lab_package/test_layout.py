#!/usr/bin/env python3

from pathlib import Path
import binascii
import hashlib
import os
import plistlib
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.layout import CodexLabAppOptions
from codex_lab_package.layout import _launcher_script
from codex_lab_package.layout import build_codex_lab_app
from codex_lab_package.smoke import smoke_check


class BuildCodexLabAppTest(unittest.TestCase):
    def _assert_valid_icns(self, contents: bytes) -> None:
        expected_chunks = (
            (b"ic10", 1024),
            (b"ic09", 512),
            (b"ic08", 256),
            (b"ic07", 128),
            (b"icp6", 64),
            (b"icp5", 32),
            (b"icp4", 16),
        )

        self.assertGreaterEqual(len(contents), 8)
        self.assertEqual(contents[:4], b"icns")
        self.assertEqual(struct.unpack_from(">I", contents, 4)[0], len(contents))

        offset = 8
        for expected_type, expected_size in expected_chunks:
            self.assertLessEqual(offset + 8, len(contents))
            self.assertEqual(contents[offset : offset + 4], expected_type)
            chunk_length = struct.unpack_from(">I", contents, offset + 4)[0]
            self.assertGreaterEqual(chunk_length, 8)
            chunk_end = offset + chunk_length
            self.assertLessEqual(chunk_end, len(contents))
            self._assert_valid_png(contents[offset + 8 : chunk_end], expected_size)
            offset = chunk_end

        self.assertEqual(offset, len(contents))

    def _assert_valid_png(self, png: bytes, size: int) -> None:
        self.assertGreaterEqual(len(png), 8)
        self.assertEqual(png[:8], b"\x89PNG\r\n\x1a\n")

        chunks = []
        offset = 8
        while offset < len(png):
            self.assertLessEqual(offset + 12, len(png))
            payload_length = struct.unpack_from(">I", png, offset)[0]
            chunk_type = png[offset + 4 : offset + 8]
            chunk_end = offset + 12 + payload_length
            self.assertLessEqual(chunk_end, len(png))
            payload = png[offset + 8 : chunk_end - 4]
            expected_checksum = struct.unpack_from(">I", png, chunk_end - 4)[0]
            checksum = binascii.crc32(chunk_type)
            checksum = binascii.crc32(payload, checksum) & 0xFFFFFFFF
            self.assertEqual(checksum, expected_checksum)
            chunks.append((chunk_type, payload))
            offset = chunk_end

        self.assertEqual(offset, len(png))
        self.assertEqual(
            [chunk_type for chunk_type, _payload in chunks], [b"IHDR", b"IDAT", b"IEND"]
        )
        header = chunks[0][1]
        self.assertEqual(len(header), 13)
        self.assertEqual(
            struct.unpack(">IIBBBBB", header),
            (size, size, 8, 6, 0, 0, 0),
        )
        self.assertEqual(chunks[-1][1], b"")

        raw = zlib.decompress(chunks[1][1])
        row_length = 1 + 4 * size
        self.assertEqual(len(raw), size * row_length)
        self.assertTrue(all(raw[row * row_length] == 0 for row in range(size)))

    def test_builds_launcher_bundle_with_embedded_cli_and_shim(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex_bin = root / "fake-codex"
            codex_bin.write_text(
                f"""#!/bin/sh
if [ "$*" = "debug provenance --json" ]; then
  printf '{{"schema_version":2,"version":"9.9.9","release_version":"1.2.3-lab.5","compatibility_version":"9.9.9","source_commit":"{"a" * 40}","dirty_state":"clean","build_profile":"release","build_channel":"lab","executable_path":"%s"}}\\n' "$0"
  exit 0
fi
exec /bin/sh "$@"
""",
                encoding="utf-8",
            )
            os.chmod(codex_bin, 0o755)

            result = build_codex_lab_app(
                CodexLabAppOptions(
                    app_dir=root / "Codex Lab.app",
                    codex_bin=codex_bin,
                    codex_app_path=Path("/custom/OpenAI Desktop.app"),
                    shim_dir=root / "bin",
                    bundle_identifier="dev.example.codex-lab-test",
                    short_version="1.2.3",
                    release_version="1.2.3-lab.5",
                    bundle_version="42",
                    embedded_cli_version="1.2.3-lab.5",
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
                    "CodexLabReleaseVersion": "1.2.3-lab.5",
                    "LSMinimumSystemVersion": "13.0",
                    "NSHighResolutionCapable": True,
                },
            )

            launcher = result.launcher_path.read_text(encoding="utf-8")
            icon = root / "Codex Lab.app/Contents/Resources/CodexLab.icns"
            self._assert_valid_icns(icon.read_bytes())
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
            with self.assertRaisesRegex(
                ValueError,
                "embedded CLI compatibility version does not match",
            ):
                smoke_check(
                    result.app_dir,
                    result.shim_path,
                    expected_compatibility_version="1.2.3",
                )

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
            source_commit = "a" * 64

            embedded_cli.write_text(
                """#!/bin/sh
set -eu
if [ "${1:-}" = debug ] && [ "${2:-}" = provenance ] && [ "${3:-}" = --json ]; then
  executable_path=${PROVENANCE_EXECUTABLE_PATH:-$0}
  printf '{"schema_version":2,"version":"1.2.3","release_version":"1.2.3-lab.5","compatibility_version":"1.2.3","source_commit":"%s","dirty_state":"clean","build_profile":"release","build_channel":"lab","executable_path":"%s"}\\n' "__SOURCE_COMMIT__" "$executable_path"
  exit 0
fi
case " $* " in
  *" app-server "*) printf '%s\\n' "$0" > "$CHILD_LOG" ;;
  *) exit 2 ;;
esac
""".replace("__SOURCE_COMMIT__", source_commit),
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
            fake_id = root / "id"
            fake_id.write_text("#!/bin/sh\nprintf '501\\n'\n", encoding="utf-8")
            fake_launchctl = root / "launchctl"
            fake_launchctl.write_text(
                """#!/bin/sh
[ "${SUPERVISOR_LAUNCHCTL_FAILURE:-}" != 1 ] || exit 1
cat <<EOF
gui/501/dev.everycode.codex-lab.app-server.v1 = {
  state = ${SUPERVISOR_STATE:-running}
  program = $SUPERVISOR_RUNNER
  pid = ${SUPERVISOR_PID:-4321}
}
EOF
""",
                encoding="utf-8",
            )
            fake_lsof = root / "lsof"
            fake_lsof.write_text(
                """#!/bin/sh
case " $* " in
  *" -d txt "*)
    printf 'p%s\\nftxt\\nn%s\\n' "$SUPERVISOR_PID" "$MANAGED_CLI"
    ;;
  *" -sTCP:LISTEN "*)
    [ "${LISTENER_MISSING:-}" != 1 ] || exit 1
    printf 'p%s\\nf28\\n' "${LISTENER_PID:-$SUPERVISOR_PID}"
    ;;
  *) exit 1 ;;
esac
""",
                encoding="utf-8",
            )
            fake_ps = root / "ps"
            fake_ps.write_text(
                """#!/bin/sh
[ "${SUPERVISOR_PS_FAILURE:-}" != 1 ] || exit 1
printf '%s\\n' "${SERVER_COMMAND:-$MANAGED_CLI app-server --remote-control --listen ws://127.0.0.1:4766}"
""",
                encoding="utf-8",
            )
            open_log = root / "open.log"
            fake_open = root / "open"
            fake_open.write_text(
                """#!/bin/sh
args="$*"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --env)
      export "$2"
      shift 2
      ;;
    *) shift ;;
  esac
done
printf 'cli=%s\\nforce_cli=%s\\ncodex_home=%s\\nlab_home=%s\\nlocal_daemon=%s\\nwebsocket=%s\\nargs=%s\\n' \\
  "${CODEX_CLI_PATH:-}" "${CODEX_APP_SERVER_FORCE_CLI:-}" \\
  "$CODEX_HOME" "$CODEX_LAB_HOME" "$CODEX_APP_SERVER_USE_LOCAL_DAEMON" \\
  "$CODEX_APP_SERVER_WS_URL" "$args" > "$OPEN_LOG"
""",
                encoding="utf-8",
            )
            for executable in (
                fake_plutil,
                fake_codesign,
                fake_id,
                fake_launchctl,
                fake_lsappinfo,
                fake_lsof,
                fake_ps,
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
                    expected_source_commit=source_commit,
                    expected_cli_version="1.2.3-lab.5",
                    expected_compatibility_version="1.2.3",
                    codesign_path=fake_codesign,
                    id_path=fake_id,
                    launchctl_path=fake_launchctl,
                    lsappinfo_path=fake_lsappinfo,
                    lsof_path=fake_lsof,
                    open_path=fake_open,
                    plutil_path=fake_plutil,
                    ps_path=fake_ps,
                    shasum_path=fake_shasum,
                ),
                encoding="utf-8",
            )
            os.chmod(launcher, 0o755)
            environment = {
                **os.environ,
                "CODEX_APP_SERVER_FORCE_CLI": "1",
                "CODEX_CLI_PATH": "/tmp/force-stdio",
                "CODEX_LAB_HOME": str(root / "Codex Lab Home"),
                "OPEN_LOG": str(open_log),
                "SUPERVISOR_PID": str(os.getpid()),
            }
            managed_cli = (
                Path(environment["CODEX_LAB_HOME"])
                / "packages/standalone/current/codex"
            )
            managed_cli.parent.mkdir(parents=True)
            managed_cli.write_bytes(embedded_cli.read_bytes())
            os.chmod(managed_cli, 0o755)
            supervisor_runner = (
                Path(environment["CODEX_LAB_HOME"])
                / "supervisor/v1/codex-lab-app-server"
            )
            supervisor_runner.parent.mkdir(parents=True)
            supervisor_runner.write_text(
                """#!/bin/sh
if [ "${1:-}" = check ] && [ "${SUPERVISOR_CHECK_FAILURE:-}" != 1 ]; then
  exit 0
fi
exit 1
""",
                encoding="utf-8",
            )
            os.chmod(supervisor_runner, 0o755)
            environment["MANAGED_CLI"] = str(managed_cli)
            environment["SUPERVISOR_RUNNER"] = str(supervisor_runner)

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
            self.assertIn("commit=" + source_commit, completed.stderr)
            self.assertNotIn("executable_path", completed.stderr)
            open_contents = open_log.read_text(encoding="utf-8")
            self.assertIn("cli=\n", open_contents)
            self.assertIn("force_cli=\n", open_contents)
            self.assertIn(f"codex_home={environment['CODEX_LAB_HOME']}", open_contents)
            self.assertIn(f"lab_home={environment['CODEX_LAB_HOME']}", open_contents)
            self.assertIn("local_daemon=\n", open_contents)
            self.assertIn("websocket=ws://127.0.0.1:4766/rpc", open_contents)
            self.assertIn("--env CODEX_CLI_PATH=", open_contents)
            self.assertIn("--env CODEX_APP_SERVER_FORCE_CLI=", open_contents)
            self.assertIn(
                f"--env CODEX_HOME={environment['CODEX_LAB_HOME']}", open_contents
            )
            self.assertIn(
                f"--env CODEX_LAB_HOME={environment['CODEX_LAB_HOME']}",
                open_contents,
            )
            self.assertIn("--env CODEX_APP_SERVER_USE_LOCAL_DAEMON=", open_contents)
            self.assertIn(
                "--env CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:4766/rpc",
                open_contents,
            )
            self.assertIn(str(official_app), open_contents)

            open_log.unlink()
            environment["PROVENANCE_EXECUTABLE_PATH"] = str(root / "wrong-codex")
            completed = subprocess.run(
                [str(launcher)],
                check=False,
                env=environment,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn(
                "provenance does not match the embedded executable", completed.stderr
            )
            self.assertFalse(open_log.exists())

            environment.pop("PROVENANCE_EXECUTABLE_PATH")
            environment["SUPERVISOR_CHECK_FAILURE"] = "1"
            completed = subprocess.run(
                [str(launcher)],
                check=False,
                env=environment,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn("app-server supervisor validation failed", completed.stderr)
            self.assertFalse(open_log.exists())

            environment.pop("SUPERVISOR_CHECK_FAILURE")
            environment["SUPERVISOR_STATE"] = "waiting"
            completed = subprocess.run(
                [str(launcher)],
                check=False,
                env=environment,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn("app-server supervisor is not running", completed.stderr)
            self.assertFalse(open_log.exists())

            environment.pop("SUPERVISOR_STATE")
            environment["LISTENER_PID"] = "9876"
            completed = subprocess.run(
                [str(launcher)],
                check=False,
                env=environment,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn("does not own ws://127.0.0.1:4766", completed.stderr)
            self.assertFalse(open_log.exists())

            environment.pop("LISTENER_PID")
            managed_contents = managed_cli.read_text(encoding="utf-8")
            managed_cli.write_text(
                managed_contents.replace(source_commit, "b" * len(source_commit)),
                encoding="utf-8",
            )
            completed = subprocess.run(
                [str(launcher)],
                check=False,
                env=environment,
                stderr=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.returncode, 1)
            self.assertIn(
                "Managed Codex Lab engine build does not match", completed.stderr
            )
            self.assertFalse(open_log.exists())
            managed_cli.write_text(managed_contents, encoding="utf-8")

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
            source_commit = "a" * 64
            codex_bin.write_text(
                """#!/bin/sh
if [ "${1:-}" = debug ] && [ "${2:-}" = provenance ] && [ "${3:-}" = --json ]; then
  printf '{"schema_version":2,"version":"1.2.3","release_version":"1.2.3-lab.5","compatibility_version":"1.2.3","source_commit":"%s","dirty_state":"clean","build_profile":"release","build_channel":"lab","executable_path":"%s"}\\n' "__SOURCE_COMMIT__" "$0"
  exit 0
fi
exit 2
""".replace("__SOURCE_COMMIT__", source_commit),
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
            self.assertIn("EXPECTED_CLI_VERSION='1.2.3-lab.5'", launcher)
            self.assertIn("EXPECTED_COMPATIBILITY_VERSION='1.2.3'", launcher)
            self.assertIn("EXPECTED_SOURCE_COMMIT='" + source_commit + "'", launcher)
            self.assertIn("Built Codex Lab app bundle", completed.stdout)

    def test_build_script_allows_gui_short_version_to_differ_from_cli_version(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex_bin = root / "fake-codex"
            source_commit = "a" * 64
            codex_bin.write_text(
                """#!/bin/sh
if [ "${1:-}" = debug ] && [ "${2:-}" = provenance ] && [ "${3:-}" = --json ]; then
  printf '{"schema_version":2,"version":"1.2.3","release_version":"1.2.3-lab.5","compatibility_version":"1.2.3","source_commit":"%s","dirty_state":"clean","build_profile":"release","build_channel":"lab","executable_path":"%s"}\\n' "__SOURCE_COMMIT__" "$0"
  exit 0
fi
exit 2
""".replace("__SOURCE_COMMIT__", source_commit),
                encoding="utf-8",
            )
            os.chmod(codex_bin, 0o755)
            app_dir = root / "Codex Lab.app"
            script = Path(__file__).resolve().parents[1] / "build_codex_lab_app.py"

            subprocess.run(
                [
                    sys.executable,
                    str(script),
                    "--codex-bin",
                    str(codex_bin),
                    "--app-dir",
                    str(app_dir),
                    "--short-version",
                    "2026.7.22",
                    "--embedded-cli-version",
                    "1.2.3-lab.5",
                ],
                check=True,
                capture_output=True,
                text=True,
            )

            with (app_dir / "Contents/Info.plist").open("rb") as handle:
                info = plistlib.load(handle)
            self.assertEqual(info["CFBundleShortVersionString"], "2026.7.22")
            launcher = (app_dir / "Contents/MacOS/Codex Lab Launcher").read_text(
                encoding="utf-8"
            )
            self.assertIn("EXPECTED_CLI_VERSION='1.2.3-lab.5'", launcher)
            self.assertIn("EXPECTED_COMPATIBILITY_VERSION='2026.7.22'", launcher)

    def test_build_script_rejects_metadata_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            codex_bin = root / "fake-codex"
            source_commit = "a" * 64
            codex_bin.write_text(
                """#!/bin/sh
printf '{"schema_version":2,"version":"1.2.3","release_version":"1.2.3-lab.5","compatibility_version":"1.2.3","source_commit":"%s","dirty_state":"clean","build_profile":"release","build_channel":"lab","executable_path":"%s"}\\n' "__SOURCE_COMMIT__" "$0"
""".replace("__SOURCE_COMMIT__", source_commit),
                encoding="utf-8",
            )
            os.chmod(codex_bin, 0o755)
            script = Path(__file__).resolve().parents[1] / "build_codex_lab_app.py"

            for option, value, message in (
                (
                    "--embedded-cli-version",
                    "9.9.9",
                    "Requested embedded CLI version",
                ),
                ("--source-commit", "b" * 64, "Requested source commit"),
            ):
                with self.subTest(option=option):
                    completed = subprocess.run(
                        [
                            sys.executable,
                            str(script),
                            "--codex-bin",
                            str(codex_bin),
                            "--app-dir",
                            str(root / "Codex Lab.app"),
                            option,
                            value,
                        ],
                        check=False,
                        capture_output=True,
                        text=True,
                    )

                    self.assertEqual(completed.returncode, 2)
                    self.assertIn(message, completed.stderr)
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
            with self.assertRaisesRegex(ValueError, "40- or 64-character hex SHA"):
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
