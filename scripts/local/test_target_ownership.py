import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import stat
import tempfile
import time
import unittest
from unittest import mock
import signal
from collections.abc import Callable
from types import SimpleNamespace
from typing import cast


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

    @staticmethod
    def wait_for(path: Path, timeout: float = 5) -> None:
        deadline = time.monotonic() + timeout
        while not path.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        if not path.exists():
            raise AssertionError(f"timed out waiting for {path}")

    def lease_descendant(
        self,
        root: Path,
        *,
        detached: bool,
        supervisor_lives: bool,
        child_closes_fd: bool = False,
    ) -> tuple[subprocess.Popen[bytes], Path, Path, Path, Path]:
        child_pid = root / "child.pid"
        child_done = root / "child.done"
        stop = root / "stop"
        supervisor_pid = root / "supervisor.pid"
        supervisor_done = root / "supervisor.done"
        child_code = (
            "import os, time\n"
            "from pathlib import Path\n"
            + (
                f"os.close(int(os.environ[{OWNERSHIP.TARGET_LEASE_FD_ENV!r}]))\n"
                if child_closes_fd
                else ""
            )
            + f"Path({str(child_pid)!r}).write_text(str(os.getpid()))\n"
            + f"done = Path({str(child_done)!r})\n"
            f"stop = Path({str(stop)!r})\n"
            "deadline = time.monotonic() + 10\n"
            "try:\n"
            "    while not stop.exists() and time.monotonic() < deadline:\n"
            "        time.sleep(.01)\n"
            "finally:\n"
            "    done.write_text(str(os.getpid()))\n"
        )
        supervisor_code = (
            "import os, subprocess, sys, time\n"
            "from pathlib import Path\n"
            f"fd = int(os.environ[{OWNERSHIP.TARGET_LEASE_FD_ENV!r}])\n"
            f"child = subprocess.Popen([sys.executable, '-c', {child_code!r}], "
            f"pass_fds=(fd,), start_new_session={detached!r})\n"
            + ("os.close(fd)\n" if supervisor_lives else "")
            + f"Path({str(supervisor_pid)!r}).write_text(str(os.getpid()))\n"
            + "deadline = time.monotonic() + 10\n"
            + (
                f"while not Path({str(stop)!r}).exists() and time.monotonic() < deadline: time.sleep(.01)\n"
                if supervisor_lives
                else ""
            )
            + f"Path({str(supervisor_done)!r}).write_text('done')\n"
        )
        owner = subprocess.Popen(
            self.owner_command(root, "build", supervisor_code),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.wait_for(child_pid)
        return owner, child_pid, child_done, stop, supervisor_pid

    @staticmethod
    def wait_for_exit(pid_path: Path, timeout: float = 5) -> None:
        pid = int(pid_path.read_text())
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.01)
        raise AssertionError(f"timed out waiting for PID {pid} to exit")

    @staticmethod
    def stop_descendants(
        child_pid: Path,
        child_done: Path,
        stop: Path,
        supervisor_pid: Path | None = None,
        owner: subprocess.Popen[bytes] | None = None,
    ) -> None:
        stop.write_text("stop")
        deadline = time.monotonic() + 5
        while not child_done.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        supervisor_done = (
            supervisor_pid.parent / "supervisor.done"
            if supervisor_pid is not None
            else None
        )
        if child_done.exists() and supervisor_done is not None:
            deadline = time.monotonic() + 5
            while not supervisor_done.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
        if not child_done.exists() or (
            supervisor_done is not None and not supervisor_done.exists()
        ):
            pid_paths = [child_pid]
            if supervisor_pid is not None:
                pid_paths.append(supervisor_pid)
            for pid_path in pid_paths:
                try:
                    os.kill(int(pid_path.read_text()), signal.SIGTERM)
                except (FileNotFoundError, ProcessLookupError):
                    pass
        TargetOwnershipTest.wait_for_exit(child_pid)
        if supervisor_pid is not None:
            TargetOwnershipTest.wait_for_exit(supervisor_pid)
        if owner is not None:
            try:
                owner.wait(timeout=5)
            except subprocess.TimeoutExpired:
                owner.terminate()
                owner.wait(timeout=5)

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

        def spawn(command: list[str], **kwargs: object) -> SimpleNamespace:
            self.assertEqual(["fake"], command)
            self.assertEqual((), kwargs["pass_fds"])
            handler = handlers[signal.SIGTERM]
            assert callable(handler)
            cast(Callable[[int, object], None], handler)(signal.SIGTERM, None)
            return SimpleNamespace(
                poll=lambda: None,
                send_signal=lambda received: sent.append(received),
                wait=lambda: 0,
            )

        with (
            mock.patch.object(signal, "signal", side_effect=install),
            mock.patch.object(subprocess, "Popen", side_effect=spawn),
        ):
            result = OWNERSHIP.execute_command(["fake"], Path("/target"))
        self.assertEqual(128 + signal.SIGTERM, result)
        self.assertEqual([signal.SIGTERM], sent)

    def test_unlocked_active_claim_is_unknown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            command = self.owner_command(
                root,
                "build",
                "import os; "
                f"os.close(int(os.environ[{OWNERSHIP.TARGET_LEASE_FD_ENV!r}])); "
                f"from pathlib import Path; Path({str(root / 'descriptor-closed')!r}).write_text('yes'); "
                "import time; time.sleep(.3)",
            )
            owner = subprocess.Popen(command)
            claim_path = root / OWNERSHIP.REGISTRY_NAME
            self.wait_for(root / "descriptor-closed")
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

    def test_foreground_exit_keeps_inherited_descendant_lease(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            owner, child_pid, child_done, stop, _ = self.lease_descendant(
                root, detached=False, supervisor_lives=False
            )
            try:
                owner.wait(timeout=5)
                inspected, result = self.inspect(root, "build")
                self.assertEqual(OWNERSHIP.EXIT_RELEASED, inspected.returncode)
                self.assertEqual(
                    {"released-unverified", "lease-held-after-release"},
                    {result["status"], result["reason"]},
                )
            finally:
                self.stop_descendants(child_pid, child_done, stop, owner=owner)
            self.assertEqual(
                OWNERSHIP.EXIT_RELEASED, self.inspect(root, "build")[0].returncode
            )

    def test_supervisor_sigkill_keeps_descendant_lease(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            owner, child_pid, child_done, stop, supervisor_pid = self.lease_descendant(
                root, detached=False, supervisor_lives=True
            )
            try:
                self.wait_for(supervisor_pid)
                owner.kill()
                owner.wait(timeout=5)
                inspected, result = self.inspect(root, "build")
                self.assertEqual(OWNERSHIP.EXIT_ACTIVE, inspected.returncode)
                self.assertEqual(
                    {"active", "lock-held"}, {result["status"], result["reason"]}
                )
            finally:
                self.stop_descendants(
                    child_pid, child_done, stop, supervisor_pid, owner
                )
            self.assertEqual(
                OWNERSHIP.EXIT_UNKNOWN, self.inspect(root, "build")[0].returncode
            )

    def test_detached_descendant_keeps_lease_after_foreground_exit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            owner, child_pid, child_done, stop, _ = self.lease_descendant(
                root, detached=True, supervisor_lives=False
            )
            try:
                owner.wait(timeout=5)
                inspected, result = self.inspect(root, "build")
                self.assertEqual(OWNERSHIP.EXIT_RELEASED, inspected.returncode)
                self.assertEqual(
                    {"released-unverified", "lease-held-after-release"},
                    {result["status"], result["reason"]},
                )
            finally:
                self.stop_descendants(child_pid, child_done, stop, owner=owner)
            self.assertEqual(
                OWNERSHIP.EXIT_RELEASED, self.inspect(root, "build")[0].returncode
            )

    def test_fd_closing_descendant_remains_unverified_when_lock_is_free(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            owner, child_pid, child_done, stop, _ = self.lease_descendant(
                root, detached=False, supervisor_lives=False, child_closes_fd=True
            )
            try:
                owner.wait(timeout=5)
                inspected, result = self.inspect(root, "build")
                self.assertEqual(OWNERSHIP.EXIT_RELEASED, inspected.returncode)
                self.assertEqual(
                    {"released-unverified", "claim-released-lock-free"},
                    {result["status"], result["reason"]},
                )
            finally:
                self.stop_descendants(child_pid, child_done, stop, owner=owner)

    def test_nested_run_fails_closed_and_preserves_outer_lease(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            inner_root = root / "inner-root"
            inner_root.mkdir()
            result_path = root / "nested-result"
            stop = root / "stop"
            code = (
                "import os, subprocess, sys, time\n"
                "from pathlib import Path\n"
                f"result = subprocess.run([sys.executable, {str(MODULE_PATH)!r}, "
                f"'run', '--root', {str(inner_root)!r}, '--target', 'inner', '--', "
                "sys.executable, '-c', 'pass'], capture_output=True, text=True)\n"
                f"Path({str(result_path)!r}).write_text(str(result.returncode) + result.stderr)\n"
                f"while not Path({str(stop)!r}).exists(): time.sleep(.01)\n"
            )
            owner = subprocess.Popen(self.owner_command(root, "build", code))
            try:
                self.wait_for(result_path)
                nested_result = result_path.read_text()
                self.assertTrue(nested_result.startswith(f"{OWNERSHIP.EXIT_UNKNOWN}"))
                self.assertIn("nested-lease-unsupported", nested_result)
                inspected, result = self.inspect(root, "build")
                self.assertEqual(OWNERSHIP.EXIT_ACTIVE, inspected.returncode)
                self.assertEqual(
                    {"active", "lock-held"}, {result["status"], result["reason"]}
                )
                self.assertFalse((inner_root / "inner").exists())
                self.assertFalse((inner_root / OWNERSHIP.REGISTRY_NAME).exists())
            finally:
                stop.write_text("stop")
                try:
                    owner.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    owner.terminate()
                    owner.wait(timeout=5)
            self.assertEqual(
                OWNERSHIP.EXIT_RELEASED, self.inspect(root, "build")[0].returncode
            )

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
