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
from codex_lab_package.supervisor import build_launch_agent_plist
from codex_lab_package.supervisor import build_supervisor_runner
from codex_lab_package.supervisor import inspect_engine
from codex_lab_package.supervisor import install_supervisor


class SupervisorTest(unittest.TestCase):
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

    def test_runner_and_plist_pin_engine_without_updater(self) -> None:
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
            self.assertIn("app-server daemon start", runner)
            self.assertIn("app-server daemon version", runner)
            self.assertNotIn("daemon bootstrap", runner)
            self.assertNotIn('"$MANAGED_CLI" app-server daemon pid-update-loop', runner)
            self.assertNotIn("ChatGPT", runner)

            plist = plistlib.loads(build_launch_agent_plist(paths))
            self.assertEqual(plist["Label"], paths.label)
            self.assertEqual(plist["ProgramArguments"], [str(paths.runner), "run"])
            self.assertTrue(plist["KeepAlive"])

    def test_inspect_engine_records_signature_and_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            engine = root / "codex"
            source_commit = "c" * 40
            engine.write_text(
                """#!/bin/sh
if [ "${1:-}" = debug ] && [ "${2:-}" = provenance ]; then
  printf '{"schema_version":1,"version":"1.2.3","source_commit":"%s","dirty_state":"clean","build_profile":"release","build_channel":"release","executable_path":"%s"}\\n' "__COMMIT__" "$0"
  exit 0
fi
exit 2
""".replace("__COMMIT__", source_commit),
                encoding="utf-8",
            )
            os.chmod(engine, 0o755)
            codesign = root / "codesign"
            codesign.write_text(
                """#!/bin/sh
if [ "${1:-}" = --verify ]; then
  exit 0
fi
echo 'Identifier=dev.example.codex-lab' >&2
echo 'TeamIdentifier=TEAM123456' >&2
""",
                encoding="utf-8",
            )
            os.chmod(codesign, 0o755)

            identity = inspect_engine(engine, codesign_path=codesign)
            self.assertEqual(identity.source_commit, source_commit)
            self.assertEqual(
                identity.sha256, hashlib.sha256(engine.read_bytes()).hexdigest()
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
                patch(
                    "codex_lab_package.supervisor._launchctl_loaded",
                    return_value=False,
                ),
                patch("codex_lab_package.supervisor._launchctl") as launchctl_call,
                patch("codex_lab_package.supervisor._wait_for_health"),
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
            self.assertTrue(paths.runner.is_file())
            self.assertTrue(paths.plist.is_file())
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
