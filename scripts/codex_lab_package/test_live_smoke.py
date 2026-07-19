from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.live_smoke import matching_app_server_pids
from codex_lab_package.live_smoke import process_has_ancestor
from codex_lab_package.live_smoke import validate_cli_provenance


class LiveSmokeTest(unittest.TestCase):
    def test_provenance_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            cli_path = Path(temp_dir) / "codex-lab"
            provenance = {
                "schema_version": 1,
                "version": "0.0.0",
                "source_commit": "a" * 40,
                "dirty_state": "clean",
                "build_profile": "release",
                "build_channel": "lab",
                "executable_path": str(cli_path),
            }
            validate_cli_provenance(provenance, cli_path)
            for field, value, message in (
                ("schema_version", 2, "schema"),
                ("source_commit", "bad", "malformed"),
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

    def test_process_and_launcher_proof_helpers(self) -> None:
        cli_path = Path("/tmp/Codex Lab.app/Contents/Resources/codex-lab")
        rows = [
            (10, 1, "/Applications/ChatGPT.app/Contents/Resources/codex app-server"),
            (20, 1, "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT"),
            (21, 20, f"{cli_path} -c features.code_mode_host=true app-server"),
        ]
        paths = {
            10: Path("/Applications/ChatGPT.app/Contents/Resources/codex"),
            21: cli_path.resolve(),
        }
        self.assertEqual(matching_app_server_pids(rows, cli_path, paths.get), [21])
        self.assertTrue(process_has_ancestor(21, {20}, rows))
