import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import stat
import tempfile
import time
import unittest
from unittest import mock
import signal
from types import SimpleNamespace


MODULE_PATH = Path(__file__).with_name("target_ownership.py")
SPEC = importlib.util.spec_from_file_location("target_ownership", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {MODULE_PATH}")
OWNERSHIP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(OWNERSHIP)


class TargetOwnershipTest(unittest.TestCase):
    @staticmethod
    def owner_command(root: Path, target: str, code: str) -> list[str]:
        return [
            sys.executable,
            str(MODULE_PATH),
            "run",
            "--root",
            str(root),
            "--target",
            target,
            "--",
            sys.executable,
            "-c",
            code,
        ]

    @staticmethod
    def run_cli(
        root: Path, subcommand: str, target: str, *args: str
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(MODULE_PATH),
                subcommand,
                "--root",
                str(root),
                "--target",
                target,
                *args,
            ],
            capture_output=True,
            text=True,
            check=False,
        )

    def inspect(
        self, root: Path, target: str
    ) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
        completed = self.run_cli(root, "inspect", target)
        return completed, json.loads(completed.stdout)

    def test_run_creates_and_releases_unverified_target(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            completed = self.run_cli(
                root,
                "run",
                "build",
                "--",
                sys.executable,
                "-c",
                "import os; from pathlib import Path; Path(os.environ['CODEX_LAB_OWNED_TARGET'], 'consumer').write_text('ok')",
            )
            self.assertEqual(0, completed.returncode, completed.stderr)
            self.assertTrue((root / "build" / "consumer").exists())
            claim = json.loads(
                next((root / OWNERSHIP.REGISTRY_NAME).glob("*.json")).read_text()
            )
            self.assertEqual("weak", claim["targetIdentity"]["strength"])
            inspected, result = self.inspect(root, "build")
            self.assertEqual(OWNERSHIP.EXIT_RELEASED, inspected.returncode)
            self.assertEqual("claim-released-lock-free", result["reason"])
            key = OWNERSHIP._target_key(root, "build")
            lock_path = root / OWNERSHIP.REGISTRY_NAME / f"{key}.lock"
            lock_path.unlink()
            (root / "outside-lock").write_text("")
            lock_path.symlink_to(root / "outside-lock")
            inspected, result = self.inspect(root, "build")
            self.assertEqual(OWNERSHIP.EXIT_UNKNOWN, inspected.returncode)
            self.assertEqual("lock-identity-invalid", result["reason"])

    def test_second_run_fails_fast_and_inspect_reports_active(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            command = self.owner_command(root, "build", "import time; time.sleep(2)")
            first = subprocess.Popen(
                command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
            )
            claim = root / OWNERSHIP.REGISTRY_NAME
            deadline = time.monotonic() + 2
            while not list(claim.glob("*.json")) and time.monotonic() < deadline:
                time.sleep(0.01)
            second = self.run_cli(
                root,
                "run",
                "build",
                "--",
                sys.executable,
                "-c",
                "pass",
            )
            self.assertEqual(OWNERSHIP.EXIT_BUSY, second.returncode)
            inspected, result = self.inspect(root, "build")
            self.assertEqual(OWNERSHIP.EXIT_ACTIVE, inspected.returncode)
            self.assertEqual(
                {"active", "lock-held"}, {result["status"], result["reason"]}
            )
            first.terminate()
            first.wait(timeout=3)
            self.assertEqual(
                OWNERSHIP.EXIT_RELEASED, self.inspect(root, "build")[0].returncode
            )

    def test_signal_during_spawn_is_forwarded_after_popen(self) -> None:
        handlers: dict[int, object] = {}
        sent: list[int] = []

        def install(number: int, handler: object) -> object:
            handlers[number] = handler
            return object()

        def spawn(*_args: object, **_kwargs: object) -> SimpleNamespace:
            handler = handlers[signal.SIGTERM]
            self.assertTrue(callable(handler))
            handler(signal.SIGTERM, None)
            return SimpleNamespace(
                poll=lambda: None,
                send_signal=lambda received: sent.append(received),
                wait=lambda: 0,
            )

        with (
            mock.patch.object(signal, "signal", side_effect=install),
            mock.patch.object(subprocess, "Popen", side_effect=spawn),
        ):
            result = OWNERSHIP._execute_command(["fake"], Path("/target"))
        self.assertEqual(128 + signal.SIGTERM, result)
        self.assertEqual([signal.SIGTERM], sent)

    def test_unlocked_active_claim_is_unknown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            command = self.owner_command(root, "build", "import time; time.sleep(.3)")
            owner = subprocess.Popen(command)
            claim_path = root / OWNERSHIP.REGISTRY_NAME
            deadline = time.monotonic() + 2
            while not list(claim_path.glob("*.json")) and time.monotonic() < deadline:
                time.sleep(0.01)
            owner.kill()
            owner.wait(timeout=2)
            inspected, result = self.inspect(root, "build")
            self.assertEqual(OWNERSHIP.EXIT_UNKNOWN, inspected.returncode)
            self.assertEqual(
                {"unknown", "active-claim-lock-free"},
                {result["status"], result["reason"]},
            )

    def test_spawn_failure_releases_lock_but_keeps_active_claim(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            completed = self.run_cli(root, "run", "build", "--", "missing-command")
            self.assertEqual(OWNERSHIP.EXIT_INTERNAL, completed.returncode)
            inspected, result = self.inspect(root, "build")
            self.assertEqual(OWNERSHIP.EXIT_UNKNOWN, inspected.returncode)
            self.assertEqual(
                {"unknown", "active-claim-lock-free"},
                {result["status"], result["reason"]},
            )

    def test_owner_waits_for_synchronous_grandchild_before_release(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            child_code = (
                "import os, time\n"
                "from pathlib import Path\n"
                "gate = (Path(os.environ['CODEX_LAB_OWNED_TARGET']) / "
                "'grandchild-release')\n"
                "deadline = time.monotonic() + 60\n"
                "while not gate.exists() and time.monotonic() < deadline:\n"
                "    time.sleep(.01)\n"
            )
            code = (
                "import os, subprocess, sys; "
                "from pathlib import Path; "
                "target = Path(os.environ['CODEX_LAB_OWNED_TARGET']); "
                f"child = subprocess.Popen([sys.executable, '-c', {child_code!r}]); "
                "(target / 'grandchild-started').write_text('yes'); "
                "child.wait(); "
                "(target / 'grandchild-complete').write_text('yes')"
            )
            owner = subprocess.Popen(self.owner_command(root, "build", code))
            release = root / "build" / "grandchild-release"
            try:
                started = root / "build" / "grandchild-started"
                deadline = time.monotonic() + 10
                while not started.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)

                self.assertTrue(started.exists())
                inspected, result = self.inspect(root, "build")
                self.assertEqual(OWNERSHIP.EXIT_ACTIVE, inspected.returncode)
                self.assertEqual(
                    {"active", "lock-held"}, {result["status"], result["reason"]}
                )

                release.write_text("yes")
                owner.wait(timeout=12)
                self.assertEqual(
                    (root / "build" / "grandchild-complete").read_text(), "yes"
                )
                self.assertEqual(
                    OWNERSHIP.EXIT_RELEASED, self.inspect(root, "build")[0].returncode
                )
            finally:
                if release.parent.is_dir():
                    try:
                        release.write_text("yes")
                    except FileNotFoundError:
                        pass
                try:
                    owner.wait(timeout=12)
                except subprocess.TimeoutExpired:
                    owner.kill()
                    owner.wait(timeout=3)

    def test_malformed_claim_and_symlink_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            registry = root / OWNERSHIP.REGISTRY_NAME
            registry.mkdir(mode=0o700)
            key = OWNERSHIP._target_key(root, "build")
            for payload, reason in (
                ("{", "claim-malformed"),
                (json.dumps({"schema": 2}), "claim-unknown-schema"),
                ("x" * (OWNERSHIP.CLAIM_LIMIT + 1), "claim-oversize"),
            ):
                claim_path = registry / f"{key}.json"
                claim_path.write_text(payload, encoding="utf-8")
                claim_path.chmod(0o600)
                inspected, result = self.inspect(root, "build")
                self.assertEqual(OWNERSHIP.EXIT_UNKNOWN, inspected.returncode)
                self.assertEqual(reason, result["reason"])

            linked_root = root / "linked"
            linked_root.symlink_to(root, target_is_directory=True)
            inspected, result = self.inspect(linked_root, "build")
            self.assertEqual(OWNERSHIP.EXIT_IDENTITY, inspected.returncode)
            self.assertEqual("identity-failure", result["status"])
            self.assertEqual("root-symlink", result["reason"])

            claim_path = registry / f"{key}.json"
            claim_path.unlink()
            claim_path.symlink_to(root / "outside")
            inspected, result = self.inspect(root, "build")
            self.assertEqual(OWNERSHIP.EXIT_UNKNOWN, inspected.returncode)
            self.assertEqual("claim-identity-invalid", result["reason"])

            registry.chmod(0o755)
            completed = self.run_cli(
                root, "run", "other", "--", sys.executable, "-c", "pass"
            )
            self.assertEqual(OWNERSHIP.EXIT_IDENTITY, completed.returncode)
            self.assertEqual(0o755, stat.S_IMODE(registry.stat().st_mode))
