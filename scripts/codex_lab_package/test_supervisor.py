from pathlib import Path
import hashlib
import os
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.supervisor import EngineIdentity
from codex_lab_package.supervisor import SupervisorPaths
from codex_lab_package.supervisor import SupervisorTools
from codex_lab_package.supervisor import build_launch_agent_plist
from codex_lab_package.supervisor import build_supervisor_runner
from codex_lab_package.supervisor import inspect_engine
from codex_lab_package.supervisor import inspect_code_mode_host
from codex_lab_package.supervisor import install_supervisor


class SupervisorTest(unittest.TestCase):
    def _write_engine(self, path: Path, *, source_commit: str = "c" * 40) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            """#!/bin/sh
if [ "${1:-}" = debug ] && [ "${2:-}" = provenance ]; then
  printf '{"schema_version":1,"version":"1.2.3","source_commit":"%s","dirty_state":"clean","build_profile":"release","build_channel":"release","executable_path":"%s"}\\n' "__COMMIT__" "$0"
  exit 0
fi
exit 2
""".replace("__COMMIT__", source_commit),
            encoding="utf-8",
        )
        os.chmod(path, 0o755)

    def _write_codesign(
        self,
        path: Path,
        *,
        entitlement_value: str | None,
    ) -> None:
        entitlement_output = ""
        if entitlement_value is not None:
            entitlement_output = f"""cat <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.security.cs.allow-jit</key><{entitlement_value}/><key>com.apple.security.cs.allow-unsigned-executable-memory</key><true/></dict></plist>
EOF
"""
        path.write_text(
            """#!/bin/sh
if [ "${1:-}" = --verify ]; then
  exit 0
fi
if [ "${2:-}" = --entitlements ]; then
__ENTITLEMENT_OUTPUT__
  exit 0
fi
echo 'Identifier=dev.example.codex-lab' >&2
echo 'TeamIdentifier=TEAM123456' >&2
""".replace("__ENTITLEMENT_OUTPUT__", entitlement_output),
            encoding="utf-8",
        )
        os.chmod(path, 0o755)

    def _identity(self) -> EngineIdentity:
        return EngineIdentity(
            build_channel="release",
            build_profile="release",
            sha256="a" * 64,
            signing_identifier="dev.example.codex-lab",
            source_commit="b" * 40,
            team_identifier="TEAM123456",
            version="1.2.3",
        )

    def test_runner_and_plist_pin_direct_websocket_engine(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            paths = SupervisorPaths(
                lab_home=root / "Codex Lab Home",
                launch_agents_dir=root / "LaunchAgents",
            )
            runner = build_supervisor_runner(paths, self._identity())
            runner_path = root / "runner"
            runner_path.write_text(runner, encoding="utf-8")

            subprocess.run(["/bin/sh", "-n", str(runner_path)], check=True)
            self.assertIn("EXPECTED_SHA256=" + "a" * 64, runner)
            self.assertIn("EXPECTED_SOURCE_COMMIT=" + "b" * 40, runner)
            self.assertIn("com\\.apple\\.security\\.cs\\.allow-jit", runner)
            self.assertIn("LISTEN_URL=ws://127.0.0.1:4766", runner)
            self.assertIn("app-server --remote-control --listen", runner)
            self.assertEqual(runner.count("in ''|*[!0-9]*) return 0"), 2)
            self.assertNotIn("app-server daemon start", runner)
            self.assertNotIn('"$MANAGED_CLI" app-server daemon pid-update-loop', runner)

            plist = plistlib.loads(build_launch_agent_plist(paths))
            self.assertEqual(plist["Label"], paths.label)
            self.assertEqual(plist["ProgramArguments"], [str(paths.runner), "run"])
            self.assertTrue(plist["KeepAlive"])

    def test_inspect_engine_records_signature_and_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            engine = root / "codex"
            source_commit = "c" * 40
            self._write_engine(engine, source_commit=source_commit)
            codesign = root / "codesign"
            self._write_codesign(codesign, entitlement_value="true")

            identity = inspect_engine(engine, codesign_path=codesign)
            self.assertEqual(identity.source_commit, source_commit)
            self.assertEqual(
                identity.sha256, hashlib.sha256(engine.read_bytes()).hexdigest()
            )

    def test_inspect_engine_rejects_missing_v8_jit_entitlement(self) -> None:
        for entitlement_value in (None, "false"):
            with self.subTest(entitlement_value=entitlement_value):
                with tempfile.TemporaryDirectory() as temp_dir:
                    root = Path(temp_dir)
                    engine = root / "codex"
                    self._write_engine(engine)
                    codesign = root / "codesign"
                    self._write_codesign(
                        codesign,
                        entitlement_value=entitlement_value,
                    )

                    with self.assertRaisesRegex(ValueError, "V8 JIT entitlement"):
                        inspect_engine(engine, codesign_path=codesign)

    def test_inspects_and_pins_code_mode_host(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            paths = SupervisorPaths(
                lab_home=root / "lab",
                launch_agents_dir=root / "LaunchAgents",
            )
            self._write_engine(paths.code_mode_host)
            codesign = root / "codesign"
            self._write_codesign(codesign, entitlement_value="true")

            host_identity = inspect_code_mode_host(
                paths.code_mode_host,
                codesign_path=codesign,
            )
            runner = build_supervisor_runner(
                paths,
                self._identity(),
                code_mode_host_identity=host_identity,
            )

            self.assertEqual(
                host_identity.sha256,
                hashlib.sha256(paths.code_mode_host.read_bytes()).hexdigest(),
            )
            self.assertIn(f"CODE_MODE_HOST={paths.code_mode_host}", runner)
            self.assertIn("EXPECTED_CODE_MODE_HOST_SHA256=", runner)
            self.assertIn("allow-unsigned-executable-memory", runner)
            self.assertIn("&& verify_code_mode_host", runner)

    @unittest.skipUnless(sys.platform == "darwin", "macOS supervisor runner")
    def test_runner_check_requires_v8_jit_entitlement(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            paths = SupervisorPaths(
                lab_home=root / "lab",
                launch_agents_dir=root / "LaunchAgents",
            )
            self._write_engine(paths.managed_cli)
            codesign = root / "codesign"
            self._write_codesign(codesign, entitlement_value="true")
            identity = inspect_engine(paths.managed_cli, codesign_path=codesign)
            runner = build_supervisor_runner(
                paths,
                identity,
                tools=SupervisorTools(codesign=codesign),
            )
            paths.runner.parent.mkdir(parents=True, exist_ok=True)
            paths.runner.write_text(runner, encoding="utf-8")
            os.chmod(paths.runner, 0o755)

            self.assertEqual(subprocess.run([paths.runner, "check"]).returncode, 0)
            self._write_codesign(codesign, entitlement_value="false")
            self.assertNotEqual(
                subprocess.run([paths.runner, "check"]).returncode,
                0,
            )

    def test_install_writes_files_and_bootstraps_expected_service(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            paths = SupervisorPaths(
                lab_home=root / "lab",
                launch_agents_dir=root / "LaunchAgents",
            )
            launchctl = root / "launchctl"
            launchctl.touch()
            with (
                patch(
                    "codex_lab_package.supervisor.inspect_engine",
                    return_value=self._identity(),
                ),
                patch("codex_lab_package.supervisor._launchctl_pid", return_value=None),
                patch("codex_lab_package.supervisor._launchctl") as launchctl_call,
                patch("codex_lab_package.supervisor._wait_for_health"),
                patch(
                    "codex_lab_package.supervisor._port_is_listening",
                    return_value=False,
                ),
                patch("codex_lab_package.supervisor._remove_legacy_supervisor"),
                patch("codex_lab_package.supervisor._stop_pid_daemon"),
                patch("codex_lab_package.supervisor.subprocess.run") as run,
            ):
                run.return_value.returncode = 0
                result = install_supervisor(
                    paths,
                    expected_sha256="a" * 64,
                    expected_source_commit="b" * 40,
                    expected_version="1.2.3",
                    launchctl_path=launchctl,
                    uid=501,
                )

            self.assertEqual(result["service"], f"gui/501/{paths.label}")
            self.assertEqual(result["websocketUrl"], "ws://127.0.0.1:4766/rpc")
            self.assertTrue(paths.runner.is_file())
            run.assert_called_once_with([str(paths.runner), "check"], check=True)
            self.assertEqual(
                [call.args for call in launchctl_call.call_args_list],
                [
                    (launchctl, "bootstrap", "gui/501", str(paths.plist)),
                    (launchctl, "kickstart", "-k", f"gui/501/{paths.label}"),
                ],
            )


if __name__ == "__main__":
    unittest.main()
