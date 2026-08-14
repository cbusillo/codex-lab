from pathlib import Path
import math
import os
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.live_smoke import matching_app_server_pids
from codex_lab_package.live_smoke import _parse_process_environment
from codex_lab_package.live_smoke import desktop_transport_proof
from codex_lab_package.live_smoke import is_serving_app_server_command
from codex_lab_package.live_smoke import process_has_environment
from codex_lab_package.live_smoke import process_has_ancestor
from codex_lab_package.live_smoke import read_process_environment
from codex_lab_package.live_smoke import read_process_rows
from codex_lab_package.live_smoke import validate_cli_provenance
from codex_lab_package.live_smoke import validate_matching_build_provenance
from codex_lab_package.live_smoke import validate_timeout_seconds


class LiveSmokeTest(unittest.TestCase):
    @unittest.skipUnless(sys.platform == "darwin", "requires KERN_PROCARGS2")
    def test_reads_real_child_environment_with_spaces(self) -> None:
        process = subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(5)"],
            env={**os.environ, "CODEX_LAB_ENV_PROBE": "value with spaces"},
        )
        try:
            time.sleep(0.1)
            self.assertEqual(
                read_process_environment(process.pid)["CODEX_LAB_ENV_PROBE"],
                "value with spaces",
            )
        finally:
            process.terminate()
            process.wait(timeout=5)

    @patch("codex_lab_package.live_smoke.subprocess.check_output")
    def test_process_rows_request_untruncated_commands(self, check_output) -> None:
        check_output.return_value = "10 1 /tmp/codex app-server\n"

        self.assertEqual(read_process_rows(), [(10, 1, "/tmp/codex app-server")])
        check_output.assert_called_once_with(
            ["/bin/ps", "-ww", "-axo", "pid=,ppid=,command="], text=True
        )

    def test_timeout_and_desktop_transport_proof(self) -> None:
        validate_timeout_seconds(10.0)
        for timeout_seconds in (9.9, math.inf, math.nan):
            with self.subTest(timeout_seconds=timeout_seconds):
                with self.assertRaisesRegex(ValueError, "finite and at least"):
                    validate_timeout_seconds(timeout_seconds)

        with tempfile.TemporaryDirectory() as temp_dir:
            log_root = Path(temp_dir)
            log_path = log_root / "codex-desktop-test-42-t0-i1.log"
            log_path.write_text(
                "Transport start success connectionId=1 hostId=local transport=websocket\n"
                "initialize_handshake_result outcome=success transportKind=websocket\n",
                encoding="utf-8",
            )
            self.assertEqual(
                desktop_transport_proof([42], log_root=log_root),
                {
                    "desktopLogPath": str(log_path.resolve()),
                    "desktopTransport": "websocket",
                },
            )
            log_path.write_text("stdio_transport_spawned pid=43\n", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "bundled stdio"):
                desktop_transport_proof([42], log_root=log_root)

    def test_provenance_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            cli_path = Path(temp_dir) / "codex-lab"
            provenance = {
                "schema_version": 2,
                "version": "0.0.0",
                "release_version": "0.0.0-lab.5",
                "compatibility_version": "0.0.0",
                "source_commit": "a" * 40,
                "dirty_state": "clean",
                "build_profile": "release",
                "build_channel": "lab",
                "executable_path": str(cli_path),
            }
            for source_commit in ("a" * 40, "b" * 64):
                with self.subTest(source_commit_length=len(source_commit)):
                    validate_cli_provenance(
                        {**provenance, "source_commit": source_commit}, cli_path
                    )
            validate_cli_provenance(
                {
                    key: value
                    for key, value in provenance.items()
                    if key not in {"release_version", "compatibility_version"}
                }
                | {"schema_version": 1},
                cli_path,
            )
            for field, value, message in (
                ("schema_version", 3, "schema"),
                ("source_commit", "bad", "malformed"),
                ("source_commit", "a" * 39, "malformed"),
                ("source_commit", "a" * 65, "malformed"),
                ("dirty_state", "dirty", "clean tracked source"),
                (
                    "executable_path",
                    str(cli_path.with_name("other")),
                    "embedded binary",
                ),
            ):
                with (
                    self.subTest(field=field),
                    self.assertRaisesRegex(ValueError, message),
                ):
                    validate_cli_provenance({**provenance, field: value}, cli_path)

            build_provenance = {
                field: provenance[field]
                for field in (
                    "schema_version",
                    "version",
                    "release_version",
                    "compatibility_version",
                    "source_commit",
                    "dirty_state",
                    "build_profile",
                    "build_channel",
                )
            }
            validate_matching_build_provenance(
                build_provenance, build_provenance.copy()
            )
            with self.assertRaisesRegex(ValueError, "source_commit"):
                validate_matching_build_provenance(
                    build_provenance,
                    {**build_provenance, "source_commit": "b" * 40},
                )

    def test_process_and_launcher_proof_helpers(self) -> None:
        cli_path = Path("/tmp/Codex Lab.app/Contents/Resources/codex-lab")
        rows = [
            (10, 1, "/Applications/ChatGPT.app/Contents/Resources/codex app-server"),
            (20, 1, "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT"),
            (21, 20, f"{cli_path} -c features.code_mode_host=true app-server"),
            (22, 20, f"{cli_path} app-server daemon version"),
        ]
        paths = {
            10: Path("/Applications/ChatGPT.app/Contents/Resources/codex"),
            21: cli_path.resolve(),
        }
        self.assertEqual(matching_app_server_pids(rows, cli_path, paths.get), [21])
        self.assertTrue(is_serving_app_server_command(rows[2][2]))
        self.assertFalse(is_serving_app_server_command(rows[3][2]))
        self.assertTrue(process_has_ancestor(21, {20}, rows))
        process = {
            "CODEX_APP_SERVER_USE_LOCAL_DAEMON": "",
            "CODEX_APP_SERVER_WS_URL": "ws://127.0.0.1:4766/rpc",
            "CODEX_HOME": "/tmp/Codex Lab",
        }
        self.assertTrue(
            process_has_environment(
                20,
                {
                    "CODEX_APP_SERVER_USE_LOCAL_DAEMON": "",
                    "CODEX_APP_SERVER_WS_URL": "ws://127.0.0.1:4766/rpc",
                    "CODEX_HOME": "/tmp/Codex Lab",
                },
                lambda _pid: process,
            )
        )
        self.assertFalse(
            process_has_environment(
                20,
                {"CODEX_HOME": "/tmp/other"},
                lambda _pid: process,
            )
        )
        self.assertTrue(
            process_has_environment(
                20,
                {"CODEX_HOME": "/tmp/Codex Lab"},
                lambda _pid: process,
                forbidden={"CODEX_CLI_PATH"},
            )
        )
        self.assertFalse(
            process_has_environment(
                20,
                {"CODEX_HOME": "/tmp/Codex Lab"},
                lambda _pid: {**process, "CODEX_CLI_PATH": "/tmp/codex"},
                forbidden={"CODEX_CLI_PATH"},
            )
        )

        argument_data = (
            struct.pack("=i", 2)
            + b"/Applications/ChatGPT.app/Contents/MacOS/ChatGPT\0\0"
            + b"/Applications/ChatGPT.app/Contents/MacOS/ChatGPT\0--flag\0"
            + b"CODEX_HOME=/tmp/Codex Lab\0CODEX_CLI_PATH=\0"
            + b"CODEX_HOME=/tmp/ignored\0\0"
        )
        self.assertEqual(
            _parse_process_environment(argument_data),
            {"CODEX_HOME": "/tmp/Codex Lab", "CODEX_CLI_PATH": ""},
        )
