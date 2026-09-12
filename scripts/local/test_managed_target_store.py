import fcntl
import json
import managed_target_store as store_module
import os
from pathlib import Path
import plistlib
import signal
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from typing import cast
import unittest
from unittest import mock
from types import SimpleNamespace


MODULE_PATH = Path(__file__).with_name("managed_target_store.py")
ManagedTargetStore = store_module.ManagedTargetStore


class ManagedTargetStoreTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(dir="/private/tmp")
        self.root = Path(self.temp.name)
        self.volume_root = self.root / "volume"
        self.volume_root.mkdir()
        self.managed_root = self.volume_root / "managed"
        self.worktree = self.root / "worktree"
        self.make_worktree("worktree")
        self.config = {
            "schema": 1,
            "volume_root": str(self.volume_root),
            "volume_uuid": "abcdefab-cdef-abcd-efab-cdefabcdefab",
            "managed_root": str(self.managed_root),
            "max_age_days": 14,
            "min_free_bytes": 200 * 1024**3,
            "max_targets": 1,
        }
        self.now = time.time()
        self.statvfs_patch = mock.patch.object(
            store_module.os,
            "statvfs",
            return_value=SimpleNamespace(f_bavail=400 * 1024**3, f_frsize=1),
        )
        self.statvfs_patch.start()

    def tearDown(self) -> None:
        self.statvfs_patch.stop()
        self.temp.cleanup()

    def store(self, **updates: object) -> ManagedTargetStore:
        config = self.config | updates
        store = ManagedTargetStore(
            config,
            volume_validator=lambda path, uuid: {
                "st_dev": path.stat().st_dev,
                "st_ino": path.stat().st_ino,
                "uuid": uuid,
            },
            clock=lambda: self.now,
        )
        store.initialize()
        return store

    def make_worktree(self, name: str) -> Path:
        worktree = self.root / name
        worktree.mkdir()
        subprocess.run(["git", "init", "--quiet", str(worktree)], check=True)
        return worktree

    @staticmethod
    def wait_for(path: Path, timeout: float = 5) -> None:
        deadline = time.monotonic() + timeout
        while not path.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        if not path.exists():
            raise AssertionError(f"timed out waiting for {path}")

    @staticmethod
    def wait_exit(pid: int, timeout: float = 5) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.01)
        raise AssertionError(f"PID {pid} did not exit")

    def collect_result(
        self, store: ManagedTargetStore, *, apply: bool = False
    ) -> dict[str, object]:
        result = store.collect(apply=apply)
        self.assertIsInstance(result, dict)
        return result

    def report_list(self, report: dict[str, object], key: str) -> list[object]:
        value = report.get(key)
        if not isinstance(value, list):
            self.fail(f"report field {key!r} is not a list")
        return value

    def report_actions(self, report: dict[str, object]) -> list[dict[str, object]]:
        actions = self.report_list(report, "actions")
        if not all(isinstance(action, dict) for action in actions):
            self.fail("report actions contain a non-object entry")
        return cast(list[dict[str, object]], actions)

    def report_action_statuses(self, report: dict[str, object]) -> list[str]:
        statuses: list[str] = []
        for action in self.report_actions(report):
            status = action.get("status")
            if not isinstance(status, str):
                self.fail("report action has a non-string status")
            statuses.append(status)
        return statuses

    def report_first_reason(self, report: dict[str, object]) -> str:
        inventory = self.report_list(report, "inventory")
        if not inventory or not isinstance(inventory[0], dict):
            self.fail("report inventory has no object entry")
        first = cast(dict[str, object], inventory[0])
        reason = first.get("reason")
        if not isinstance(reason, str):
            self.fail("report inventory entry has a non-string reason")
        return reason

    @staticmethod
    def record(
        store: ManagedTargetStore, worktree: Path
    ) -> tuple[str, Path, dict[str, object]]:
        source = store._source(worktree, create_nonce=False)
        key = store._key(source)
        path = store._record_path(key)
        return key, path, json.loads(path.read_text())

    @staticmethod
    def save_record(
        store: ManagedTargetStore, key: str, record: dict[str, object]
    ) -> None:
        store._save_record(key, record)

    def test_initialize_and_warming_reuse(self) -> None:
        store = self.store()
        command = [
            os.fspath(sys.executable),
            "-c",
            "import os; from pathlib import Path; "
            "assert os.environ['KACHE_AUTO_GC'] == '0'; "
            "Path(os.environ['CARGO_TARGET_DIR'], 'warm').write_text('ok')",
        ]
        self.assertEqual(0, store.run(self.worktree, command, self.worktree))
        first = store.target_for(self.worktree)
        self.assertEqual(first, store.target_for(self.worktree))
        self.assertTrue((first / "warm").exists())
        self.assertEqual(0, store.run(self.worktree, command, self.worktree))
        self.assertTrue((first / "warm").exists())

    def test_stale_candidate_is_bounded_and_safe_apply_removes_only_it(self) -> None:
        store = self.store(max_targets=2)
        old_worktree = self.root / "old"
        self.make_worktree("old")
        self.assertEqual(0, store.run(old_worktree, ["true"], old_worktree))
        old_target = store.target_for(old_worktree)
        old_time = time.time() - 30 * 86400
        os.utime(old_target, (old_time, old_time))
        self.now += 30 * 86400
        result = self.collect_result(store)
        self.assertEqual(1, len(self.report_list(result, "selected_ids")))
        applied = self.collect_result(store, apply=True)
        self.assertEqual("deleted", self.report_action_statuses(applied)[0])
        self.assertFalse(old_target.exists())

    def test_new_run_below_floor_refuses_without_creating_target(self) -> None:
        store = self.store(min_free_bytes=2**63 - 1)
        with self.assertRaises(store_module.ManagedTargetError):
            store.run(self.worktree, ["true"], self.worktree)
        self.assertEqual([], list(store._targets_path.iterdir()))

    def test_symlink_replacement_and_invalid_timestamp_refuse(self) -> None:
        store = self.store()
        self.assertEqual(0, store.run(self.worktree, ["true"], self.worktree))
        target = store.target_for(self.worktree)
        key, _, record = self.record(store, self.worktree)
        original_last_used = record["last_used"]
        record["last_used"] = float("nan")
        self.save_record(store, key, record)
        result = self.collect_result(store, apply=True)
        self.assertFalse(self.report_actions(result))
        self.assertEqual("invalid-timestamp", self.report_first_reason(result))
        record["last_used"] = original_last_used
        self.save_record(store, key, record)

        replacement = self.root / "replacement"
        target.rename(replacement)
        target.mkdir()
        result = self.collect_result(store, apply=True)
        self.assertFalse(self.report_actions(result))
        target.rmdir()
        target.symlink_to(replacement, target_is_directory=True)
        result = self.collect_result(store, apply=True)
        self.assertFalse(self.report_actions(result))
        self.assertTrue(target.is_symlink())
        target.unlink()
        target.mkdir()
        old_root = self.managed_root.with_name("managed-old")
        self.managed_root.rename(old_root)
        self.managed_root.mkdir()
        with self.assertRaises(store_module.ManagedTargetError):
            store.collect()

    def test_source_and_legacy_paths_are_never_adopted(self) -> None:
        store = self.store()
        legacy = self.managed_root / "targets" / "legacy-target"
        legacy.mkdir(parents=True)
        (legacy / ".codex-target-ownership").write_text("legacy")
        source = self.root / "source-target"
        source.mkdir()
        result = self.collect_result(store, apply=True)
        self.assertFalse(self.report_actions(result))
        self.assertTrue(legacy.exists())
        self.assertTrue(source.exists())

    def test_missing_or_replaced_lock_is_protected_without_preview_mutation(
        self,
    ) -> None:
        store = self.store()
        self.assertEqual(0, store.run(self.worktree, ["true"], self.worktree))
        key, record_path, record = self.record(store, self.worktree)
        lock_path = store._targets_path / f"{key}.lock"
        before = record_path.read_bytes()
        lock_path.unlink()
        preview = self.collect_result(store)
        self.assertFalse(self.report_list(preview, "selected_ids"))
        self.assertEqual(before, record_path.read_bytes())
        lock_path.write_text("replacement")
        lock_path.chmod(0o600)
        preview = self.collect_result(store)
        self.assertFalse(self.report_list(preview, "selected_ids"))
        self.assertEqual(before, record_path.read_bytes())

    def test_interrupted_deletion_and_claimed_namespace_stay_protected(self) -> None:
        store = self.store()
        self.assertEqual(0, store.run(self.worktree, ["true"], self.worktree))
        key, _, record = self.record(store, self.worktree)
        record["state"] = "deleting"
        self.save_record(store, key, record)
        result = self.collect_result(store, apply=True)
        self.assertFalse(self.report_actions(result))
        self.assertEqual("protected-nonidle", self.report_first_reason(result))

    def test_retired_source_can_be_collected_but_replaced_source_cannot(self) -> None:
        store = self.store(max_targets=2)
        retired = self.make_worktree("retired")
        self.assertEqual(0, store.run(retired, ["true"], retired))
        retired_target = store.target_for(retired)
        retired.rename(self.root / "retired-away")
        self.now += 30 * 86400
        applied = self.collect_result(store, apply=True)
        self.assertIn("deleted", self.report_action_statuses(applied))
        self.assertFalse(retired_target.exists())

        replaced = self.make_worktree("replaced")
        self.assertEqual(0, store.run(replaced, ["true"], replaced))
        replaced_target = store.target_for(replaced)
        replaced.rename(self.root / "replaced-away")
        self.make_worktree("replaced")
        applied = self.collect_result(store, apply=True)
        self.assertFalse(self.report_actions(applied))
        self.assertTrue(replaced_target.exists())

    def test_crash_record_with_free_lease_is_still_protected(self) -> None:
        store = self.store()
        self.assertEqual(0, store.run(self.worktree, ["true"], self.worktree))
        key, _, record = self.record(store, self.worktree)
        record["state"] = "active"
        self.save_record(store, key, record)
        result = self.collect_result(store, apply=True)
        self.assertFalse(self.report_actions(result))
        self.assertEqual("protected-nonidle", self.report_first_reason(result))
        # A later explicit invocation can resume this same known target after
        # acquiring its lease; automatic collection alone cannot recover it.
        target = store.target_for(self.worktree)
        inode = target.stat().st_ino
        self.assertEqual(0, store.run(self.worktree, ["true"], self.worktree))
        self.assertEqual(inode, target.stat().st_ino)
        self.assertEqual("managed-idle", self.record(store, self.worktree)[2]["state"])

    def test_pressure_selects_at_most_configured_candidates(self) -> None:
        store = self.store(max_targets=1)
        self.assertEqual(0, store.run(self.worktree, ["true"], self.worktree))
        second = self.root / "second"
        self.make_worktree("second")
        self.assertEqual(0, store.run(second, ["true"], second))
        low_space = mock.patch.object(
            store_module.os,
            "statvfs",
            return_value=SimpleNamespace(f_bavail=1, f_frsize=1),
        )
        low_space.start()
        try:
            self.now += 2 * 86400
            result = self.collect_result(store)
        finally:
            low_space.stop()
        self.assertLessEqual(len(self.report_list(result, "selected_ids")), 1)

    def test_uuid_case_wrong_uuid_and_remount_device_fail_closed(self) -> None:
        volume_uuid = self.config.get("volume_uuid")
        if not isinstance(volume_uuid, str):
            self.fail("fixture volume UUID is not a string")
        upper = self.config | {"volume_uuid": volume_uuid.upper()}

        def validator(path: Path, value: str) -> dict[str, object]:
            return {
                "st_dev": path.stat().st_dev,
                "st_ino": path.stat().st_ino,
                "uuid": value,
            }

        upper_store = ManagedTargetStore(upper, volume_validator=validator)
        upper_store.initialize()
        self.assertEqual(upper_store.volume_uuid, self.config["volume_uuid"])

        wrong = self.config | {"managed_root": str(self.volume_root / "wrong")}
        with self.assertRaises(store_module.ManagedTargetError):
            ManagedTargetStore(
                wrong,
                volume_validator=lambda path, value: {
                    "st_dev": path.stat().st_dev,
                    "st_ino": path.stat().st_ino,
                    "uuid": "87654321-4321-8765-4321-876543218765",
                },
            ).initialize()

        remount = self.config | {"managed_root": str(self.volume_root / "remount")}
        actual_device = self.volume_root.stat().st_dev
        actual_inode = self.volume_root.stat().st_ino
        reported_device = actual_device

        def remount_validator(path: Path, value: str) -> dict[str, object]:
            return {
                "st_dev": reported_device,
                "st_ino": actual_inode,
                "uuid": value,
            }

        remount_store = ManagedTargetStore(
            remount,
            volume_validator=remount_validator,
        )
        remount_store.initialize()
        volume = remount_store._volume()
        self.assertEqual(actual_device, volume["st_dev"])
        self.assertEqual(actual_inode, volume["st_ino"])
        self.assertEqual(self.config["volume_uuid"], volume["uuid"])
        self.assertIsInstance(remount_store.collect(), dict)
        reported_device = actual_device + 1
        with self.assertRaises(store_module.ManagedTargetError):
            remount_store.collect()
        reported_device = actual_device
        self.assertIsInstance(remount_store.collect(), dict)

    @unittest.skipUnless(sys.platform == "darwin", "diskutil validation is macOS-only")
    def test_native_uuid_normalization_and_container_uuid_refusal(self) -> None:
        actual = "ABCDEFAB-CDEF-ABCD-EFAB-CDEFABCDEFAB"
        completed = SimpleNamespace(
            stdout=plistlib.dumps(
                {"MountPoint": str(self.volume_root), "VolumeUUID": actual}
            )
        )
        stat = SimpleNamespace(st_dev=1, st_ino=2)
        with (
            mock.patch.object(subprocess, "run", return_value=completed),
            mock.patch.object(os, "stat", return_value=stat),
        ):
            result = store_module.verify_volume(self.volume_root, actual.lower())
            self.assertEqual(actual.lower(), result.get("uuid"))
            with self.assertRaises(store_module.ManagedTargetError):
                store_module.verify_volume(
                    self.volume_root, "12345678-1234-5678-1234-567812345678"
                )
        container_only = SimpleNamespace(
            stdout=plistlib.dumps(
                {"MountPoint": str(self.volume_root), "ContainerUUID": actual}
            )
        )
        with mock.patch.object(subprocess, "run", return_value=container_only):
            with self.assertRaises(store_module.ManagedTargetError):
                store_module.verify_volume(self.volume_root, actual.lower())

    def test_gc_lock_closes_when_identity_probe_raises(self) -> None:
        store = self.store()
        layout = store._ensure()
        open_file = store_module._private_file
        fstat = os.fstat
        opened: list[int] = []

        def track_open(path: Path) -> int:
            fd = open_file(path)
            opened.append(fd)
            return fd

        def fail_gc_probe(fd: int) -> os.stat_result:
            if fd in opened:
                raise OSError("injected GC identity probe failure")
            return fstat(fd)

        with (
            mock.patch.object(store, "_ensure", return_value=layout),
            mock.patch.object(store_module, "_private_file", side_effect=track_open),
            mock.patch.object(os, "fstat", side_effect=fail_gc_probe),
        ):
            with self.assertRaisesRegex(OSError, "injected"):
                store.collect(apply=True)
        self.assertEqual(1, len(opened))
        with self.assertRaises(OSError):
            fstat(opened[0])

    def test_gc_lock_identity_mismatch_is_error_not_busy(self) -> None:
        store = self.store()
        layout = store._ensure()
        lock_path = store._gc_lock_path
        old_lock = self.root / "gc-lock-old"
        lock_path.rename(old_lock)
        lock_path.touch(mode=0o600)
        try:
            with mock.patch.object(store, "_ensure", return_value=layout):
                with self.assertRaisesRegex(
                    store_module.ManagedTargetError, "GC lock identity changed"
                ):
                    store.collect(apply=True)
        finally:
            lock_path.unlink()
            old_lock.rename(lock_path)

    def test_ensure_closes_gc_fd_when_identity_probe_raises(self) -> None:
        store = self.store()
        open_file = store_module._private_file
        real_fstat = os.fstat
        opened: list[int] = []

        def track_open(path: Path) -> int:
            fd = open_file(path)
            opened.append(fd)
            return fd

        def fail_probe(fd: int) -> os.stat_result:
            if fd in opened:
                raise OSError("injected ensure identity probe failure")
            return real_fstat(fd)

        with (
            mock.patch.object(store_module, "_private_file", side_effect=track_open),
            mock.patch.object(os, "fstat", side_effect=fail_probe),
        ):
            with self.assertRaisesRegex(OSError, "injected ensure"):
                store._ensure()
        self.assertEqual(1, len(opened))
        with self.assertRaises(OSError):
            real_fstat(opened[0])

    def test_inspect_record_closes_lease_fd_when_identity_probe_raises(self) -> None:
        store = self.store()
        self.assertEqual(0, store.run(self.worktree, ["true"], self.worktree))
        key, _, record = self.record(store, self.worktree)
        volume_dev = store._volume()["st_dev"]
        open_file = store_module._private_file
        real_fstat = os.fstat
        opened: list[int] = []

        def track_open(path: Path) -> int:
            fd = open_file(path)
            opened.append(fd)
            return fd

        def fail_probe(fd: int) -> os.stat_result:
            if fd in opened:
                raise OSError("injected record identity probe failure")
            return real_fstat(fd)

        with (
            mock.patch.object(store_module, "_private_file", side_effect=track_open),
            mock.patch.object(os, "fstat", side_effect=fail_probe),
        ):
            item, candidate = store._inspect_record(key, record, self.now, volume_dev)
        self.assertEqual("identity-invalid", item["reason"])
        self.assertIsNone(candidate)
        self.assertEqual(1, len(opened))
        with self.assertRaises(OSError):
            real_fstat(opened[0])

    def test_preview_returns_while_gc_lock_is_held(self) -> None:
        store = self.store()
        fd = os.open(store._gc_lock_path, os.O_RDWR)
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        result: dict[str, object] = {}

        def preview() -> None:
            try:
                result.update(store.collect())
            except BaseException as error:
                result["error"] = error

        worker = threading.Thread(target=preview)
        try:
            worker.start()
            worker.join(1)
            self.assertFalse(worker.is_alive())
            self.assertTrue(result)
            self.assertNotIn("error", result)
        finally:
            if worker.is_alive():
                fcntl.flock(fd, fcntl.LOCK_UN)
                worker.join(5)
            else:
                fcntl.flock(fd, fcntl.LOCK_UN)
            os.close(fd)

    def test_detached_holder_protects_target_after_foreground_exit(self) -> None:
        store = self.store(max_age_days=1e-9)
        ready = self.root / "ready"
        stop = self.root / "stop"
        holder = (
            "import os,time\n"
            "from pathlib import Path\n"
            f"Path({str(ready)!r}).write_text(str(os.getpid()))\n"
            f"deadline=time.monotonic()+10\n"
            f"while not Path({str(stop)!r}).exists() and time.monotonic()<deadline:\n"
            "    time.sleep(.01)\n"
        )
        foreground = (
            "import os,subprocess,sys; "
            f"fd=int(os.environ['CODEX_LAB_TARGET_LEASE_FD']); "
            f"subprocess.Popen([sys.executable,'-c',{holder!r}], pass_fds=(fd,), start_new_session=True)"
        )
        self.assertEqual(
            0,
            store.run(
                self.worktree,
                [sys.executable, "-c", foreground],
                self.worktree,
            ),
        )
        try:
            self.wait_for(ready)
            self.now += 1
            target = store.target_for(self.worktree)
            result = self.collect_result(store, apply=True)
            self.assertTrue(target.exists())
            self.assertFalse(self.report_actions(result))
        finally:
            stop.write_text("stop")
            if ready.exists():
                self.wait_exit(int(ready.read_text()))

    def test_collect_and_new_run_serialize_on_same_target_lease(self) -> None:
        store = self.store()
        self.assertEqual(0, store.run(self.worktree, ["true"], self.worktree))
        self.now += 30 * 86400
        entered = threading.Event()
        release = threading.Event()
        original_rmtree = shutil.rmtree

        def paused_rmtree(path: str, *, dir_fd: int | None = None) -> None:
            entered.set()
            self.assertTrue(release.wait(5))
            original_rmtree(path, dir_fd=dir_fd)

        paused_rmtree.avoids_symlink_attacks = getattr(
            original_rmtree, "avoids_symlink_attacks", False
        )

        result: dict[str, object] = {}
        collector = threading.Thread(
            target=lambda: result.update(store.collect(apply=True))
        )
        with mock.patch.object(shutil, "rmtree", paused_rmtree):
            collector.start()
            self.assertTrue(entered.wait(5))

            run_outcome: dict[str, BaseException] = {}

            def rerun_same_target() -> None:
                try:
                    store.run(self.worktree, ["true"], self.worktree)
                except BaseException as error:
                    run_outcome["error"] = error

            rerun = threading.Thread(target=rerun_same_target)
            rerun.start()
            time.sleep(0.1)
            self.assertIsInstance(
                run_outcome.get("error"), store_module.ManagedTargetError
            )
            release.set()
            collector.join(5)
            rerun.join(5)
        self.assertFalse(collector.is_alive())
        self.assertFalse(rerun.is_alive())
        self.assertEqual("deleted", self.report_action_statuses(result)[0])
        self.assertFalse(store.target_for(self.worktree).exists())

    def test_unrelated_target_run_can_complete_during_gc_delete(self) -> None:
        store = self.store(max_targets=2)
        old = self.make_worktree("gc-old")
        unrelated = self.make_worktree("gc-unrelated")
        self.assertEqual(0, store.run(old, ["true"], old))
        self.now += 30 * 86400
        entered, release = threading.Event(), threading.Event()
        original = shutil.rmtree

        def paused(path: str, *, dir_fd: int | None = None) -> None:
            entered.set()
            release.wait(5)
            original(path, dir_fd=dir_fd)

        paused.avoids_symlink_attacks = getattr(
            original, "avoids_symlink_attacks", False
        )
        report: dict[str, object] = {}
        with mock.patch.object(shutil, "rmtree", paused):
            gc = threading.Thread(
                target=lambda: report.update(store.collect(apply=True))
            )
            gc.start()
            self.assertTrue(entered.wait(5))
            run_result: dict[str, object] = {}

            def run_unrelated() -> None:
                try:
                    run_result["code"] = store.run(unrelated, ["true"], unrelated)
                except BaseException as error:
                    run_result["error"] = error

            worker = threading.Thread(target=run_unrelated)
            worker.start()
            worker.join(5)
            release.set()
            gc.join(5)
        self.assertEqual(0, run_result.get("code"))
        self.assertNotIn("error", run_result)
        self.assertFalse(gc.is_alive())
        self.assertEqual("deleted", self.report_action_statuses(report)[0])
        self.assertTrue(store.target_for(unrelated).exists())

    def test_supervisor_signal_keeps_successful_child_record_active(self) -> None:
        ready = self.root / "signal-ready"
        child = (
            "import signal,time\n"
            "from pathlib import Path\n"
            f"ready=Path({str(ready)!r})\n"
            "def stop(_signum, _frame):\n"
            "    ready.write_text('caught')\n"
            "    raise SystemExit(0)\n"
            "signal.signal(signal.SIGTERM, stop)\n"
            "ready.write_text('ready')\n"
            "while True: time.sleep(.1)\n"
        )
        wrapper = (
            "import importlib.util,sys\n"
            f"spec=importlib.util.spec_from_file_location('store',{str(MODULE_PATH)!r})\n"
            "module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module)\n"
            f"config={self.config!r}\n"
            "validator=lambda path,value: {'st_dev':path.stat().st_dev,'st_ino':path.stat().st_ino,'uuid':value}\n"
            "store=module.ManagedTargetStore(config, volume_validator=validator)\n"
            "store.initialize()\n"
            f"raise SystemExit(store.run({str(self.worktree)!r}, [sys.executable, '-c', {child!r}], {str(self.worktree)!r}))\n"
        )
        owner = subprocess.Popen([sys.executable, "-c", wrapper])
        try:
            self.wait_for(ready)
            owner.send_signal(signal.SIGTERM)
            owner.wait(timeout=5)
            store = self.store()
            _, _, record = self.record(store, self.worktree)
            self.assertEqual("active", record["state"])
        finally:
            if owner.poll() is None:
                owner.terminate()
                owner.wait(timeout=5)
