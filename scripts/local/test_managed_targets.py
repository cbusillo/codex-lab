import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

import managed_targets
from managed_targets import (
    CONFIG_ENV,
    LEASE_ENV,
    ManagedTargetsError,
    existing_config_path,
    main,
    managed_recipe_available,
    prewarm_kache,
    reject_managed_override,
    recipe_command,
    run_kache_gc,
    run_recipe,
    supported_recipe_arguments,
    validate_package_outputs,
)


class FakeStore:
    def __init__(self, config: dict[str, object]) -> None:
        self.config = config
        self.calls: list[tuple[Path, list[str], Path]] = []
        self.initialized = False

    def initialize(self) -> None:
        self.initialized = True

    def run(self, worktree: Path, command: list[str], cwd: Path) -> int:
        self.calls.append((worktree, command, cwd))
        return 37

    def collect(self, apply: bool = False) -> dict[str, object]:
        return {"actual_free_bytes": 400 * 1024**3, "under_pressure": False}


class PressureStore(FakeStore):
    def __init__(self, config: dict[str, object]) -> None:
        super().__init__(config)
        self.collect_calls: list[bool] = []

    def collect(self, apply: bool = False) -> dict[str, object]:
        self.collect_calls.append(apply)
        available = 100 * 1024**3
        if apply or len(self.collect_calls) > 1:
            available = 300 * 1024**3
        return {
            "actual_free_bytes": available,
            "under_pressure": not apply,
            "actions": [],
        }


class ManagedTargetsTest(unittest.TestCase):
    def clear_target_overrides(self) -> None:
        environment = mock.patch.dict(os.environ)
        environment.start()
        self.addCleanup(environment.stop)
        for name in managed_targets.TARGET_OVERRIDE_ENV_NAMES:
            os.environ.pop(name, None)

    def test_explicit_missing_config_is_an_error(self) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as temp_dir:
            missing = Path(temp_dir) / "missing.json"
            with mock.patch.dict("os.environ", {CONFIG_ENV: str(missing)}):
                with self.assertRaisesRegex(ManagedTargetsError, "missing"):
                    existing_config_path()

    def test_absent_default_config_is_unmanaged(self) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as temp_dir:
            missing = Path(temp_dir) / "missing.json"
            with (
                mock.patch.object(managed_targets, "DEFAULT_CONFIG_PATH", missing),
                mock.patch.dict("os.environ", {}, clear=True),
            ):
                self.assertIsNone(existing_config_path())

    def test_explicit_target_overrides_skip_enrollment(self) -> None:
        self.assertFalse(managed_recipe_available("test", [], {"CARGO_TARGET_DIR": ""}))
        self.assertFalse(
            managed_recipe_available(
                "test", ["--manifest-path", "other/Cargo.toml"], {}
            )
        )
        self.assertFalse(managed_recipe_available("test", ["--config=other"], {}))
        self.assertFalse(managed_recipe_available("test", [], {LEASE_ENV: "3"}))
        self.assertTrue(managed_recipe_available("test", ["-p", "codex-core"], {}))
        self.assertTrue(managed_recipe_available("build", [], {}))

    def test_unknown_flags_cannot_enter_managed_recipe(self) -> None:
        self.assertFalse(managed_recipe_available("build", ["--jobs", "4"], {}))
        self.assertFalse(supported_recipe_arguments("test", ["-Zunstable-options"]))

    def test_override_inside_managed_root_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as temp_dir:
            managed_root = Path(temp_dir) / "targets"
            with self.assertRaisesRegex(ManagedTargetsError, "inside"):
                reject_managed_override(
                    {"managed_root": str(managed_root)},
                    ["--manifest-path", str(managed_root / "Cargo.toml")],
                    {},
                )

    def test_recipe_commands_are_fixed_to_supported_entrypoints(self) -> None:
        root = Path("/repo")
        cases = {
            "test": (["test", "-p", "x"], root / "codex-rs"),
            "clippy": (["clippy", "-p", "x"], root / "codex-rs"),
            "fix": (["fix", "-p", "x"], root / "codex-rs"),
            "build": (["cargo", "build", "-p", "x"], root / "codex-rs"),
        }
        for recipe, expected in cases.items():
            with self.subTest(recipe=recipe):
                command, cwd = recipe_command(root, recipe, ["-p", "x"])
                self.assertEqual(command[-2:], expected[0][-2:])
                self.assertEqual(cwd, expected[1])

        package_command, package_cwd = recipe_command(
            root, "assemble-codex-package", ["--package-version", "1.2.3"]
        )
        self.assertEqual(
            package_command[-3:],
            ["assemble-codex-package", "--package-version", "1.2.3"],
        )
        self.assertEqual(package_cwd, root / "codex-rs")

    def test_nextest_values_allow_expressions_and_paths_with_spaces(self) -> None:
        self.assertTrue(
            supported_recipe_arguments(
                "test",
                ["-E", "package(name == 'codex core')", "--features", "one,two"],
            )
        )

    def test_run_recipe_transparently_returns_engine_status(self) -> None:
        self.clear_target_overrides()
        fake_store = FakeStore({"schema": 1, "kache_gc": False})
        with (
            mock.patch.object(
                managed_targets, "_repo_root", return_value=Path("/repo")
            ),
            mock.patch.object(managed_targets, "_store", return_value=fake_store),
        ):
            result = run_recipe(
                {"schema": 1, "kache_gc": False}, "build", ["-p", "codex-cli"]
            )
        self.assertEqual(result, 37)
        self.assertEqual(
            fake_store.calls,
            [
                (
                    Path("/repo"),
                    ["cargo", "build", "-p", "codex-cli"],
                    Path("/repo/codex-rs"),
                )
            ],
        )

    def test_run_recipe_rejects_unsupported_flag_before_engine(self) -> None:
        with mock.patch.object(managed_targets, "_store") as store:
            with self.assertRaisesRegex(ManagedTargetsError, "unsupported flags"):
                run_recipe({"schema": 1, "kache_gc": False}, "build", ["--jobs", "4"])
        store.assert_not_called()

    def test_run_recipe_performs_at_most_one_pressure_collection(self) -> None:
        self.clear_target_overrides()
        fake_store = PressureStore({"schema": 1, "kache_gc": False})
        with (
            mock.patch.object(
                managed_targets, "_repo_root", return_value=Path("/repo")
            ),
            mock.patch.object(managed_targets, "_store", return_value=fake_store),
        ):
            result = run_recipe(
                {
                    "schema": 1,
                    "kache_gc": False,
                    "min_free_bytes": 200 * 1024**3,
                },
                "build",
                [],
            )
        self.assertEqual(result, 37)
        self.assertEqual(fake_store.collect_calls, [False, True, False])

    def test_package_outputs_cannot_use_managed_root(self) -> None:
        self.clear_target_overrides()
        with tempfile.TemporaryDirectory() as temp_dir:
            managed_root = Path(temp_dir) / "targets"
            with self.assertRaisesRegex(ManagedTargetsError, "inside"):
                run_recipe(
                    {
                        "schema": 1,
                        "managed_root": str(managed_root),
                        "kache_gc": False,
                    },
                    "assemble-codex-package",
                    ["--package-dir", str(managed_root / "package")],
                )

    def test_default_package_output_is_checked_against_managed_root(self) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as temp_dir:
            managed_root = Path(temp_dir) / "targets"
            with mock.patch.object(
                managed_targets.tempfile, "gettempdir", return_value=str(managed_root)
            ):
                with self.assertRaisesRegex(
                    ManagedTargetsError, "default package output"
                ):
                    validate_package_outputs({"managed_root": str(managed_root)}, [])
            validate_package_outputs({"managed_root": str(managed_root)}, [])

    def test_config_file_is_loaded_only_when_present(self) -> None:
        with tempfile.TemporaryDirectory(dir=Path.cwd()) as temp_dir:
            path = Path(temp_dir) / "config.json"
            path.write_text(json.dumps({"schema": 1}), encoding="utf-8")
            self.assertEqual(existing_config_path(str(path)), path)

    def test_init_never_overwrites_existing_config(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = Path(temp_dir) / "config.json"
            path.write_text("existing\n", encoding="utf-8")
            with mock.patch.object(managed_targets, "_store") as store:
                self.assertEqual(
                    main(
                        [
                            "--config",
                            str(path),
                            "init",
                            "--volume-root",
                            temp_dir,
                            "--volume-uuid",
                            "00000000-0000-0000-0000-000000000000",
                            "--managed-root",
                            str(Path(temp_dir) / "targets"),
                        ]
                    ),
                    2,
                )
            store.assert_not_called()
            self.assertEqual(path.read_text(encoding="utf-8"), "existing\n")

    def test_kache_prewarm_starts_only_when_absent(self) -> None:
        status = json.dumps(
            {
                "version": "0.18.0",
                "daemon_running": False,
                "daemon_version": None,
                "service_executable_mismatch": False,
            }
        )
        ready = json.dumps(
            {
                "version": "0.18.0",
                "daemon_running": True,
                "daemon_version": "0.18.0",
                "service_executable_mismatch": False,
            }
        )
        transitional = json.dumps(
            {
                "version": "0.18.0",
                "daemon_running": True,
                "daemon_version": None,
                "service_executable_mismatch": False,
            }
        )
        completed = [
            mock.Mock(returncode=0, stdout=status, stderr=""),
            mock.Mock(returncode=0, stdout="started", stderr=""),
            mock.Mock(returncode=0, stdout=transitional, stderr=""),
            mock.Mock(returncode=0, stdout=ready, stderr=""),
        ]
        with (
            mock.patch.object(
                managed_targets.shutil, "which", return_value="/bin/kache"
            ),
            mock.patch.object(
                managed_targets.subprocess, "run", side_effect=completed
            ) as run,
        ):
            prewarm_kache({"kache_gc": True})
        start_call = run.call_args_list[1]
        self.assertEqual(start_call.args[0], ["/bin/kache", "daemon", "start"])

    def test_kache_prewarm_waits_for_running_daemon_handshake(self) -> None:
        transitional = json.dumps(
            {
                "version": "0.18.0",
                "daemon_running": True,
                "daemon_version": None,
                "service_executable_mismatch": False,
            }
        )
        ready = json.dumps(
            {
                "version": "0.18.0",
                "daemon_running": True,
                "daemon_version": "0.18.0",
                "service_executable_mismatch": False,
            }
        )
        with (
            mock.patch.object(
                managed_targets.shutil, "which", return_value="/bin/kache"
            ),
            mock.patch.object(
                managed_targets.subprocess,
                "run",
                side_effect=[
                    mock.Mock(returncode=0, stdout=transitional, stderr=""),
                    mock.Mock(returncode=0, stdout=ready, stderr=""),
                ],
            ) as run,
        ):
            prewarm_kache({"kache_gc": True})
        self.assertEqual(len(run.call_args_list), 2)

    def test_kache_version_mismatch_does_not_restart_running_daemon(self) -> None:
        status = json.dumps(
            {
                "version": "0.18.0",
                "daemon_running": True,
                "daemon_version": "0.17.0",
                "service_executable_mismatch": False,
            }
        )
        with (
            mock.patch.object(
                managed_targets.shutil, "which", return_value="/bin/kache"
            ),
            mock.patch.object(
                managed_targets.subprocess,
                "run",
                return_value=mock.Mock(returncode=0, stdout=status, stderr=""),
            ) as run,
        ):
            with self.assertRaisesRegex(ManagedTargetsError, "version mismatch"):
                prewarm_kache({"kache_gc": True})
        run.assert_called_once()

    def test_kache_status_requires_client_version(self) -> None:
        status = json.dumps(
            {
                "daemon_running": True,
                "daemon_version": None,
                "service_executable_mismatch": False,
            }
        )
        with (
            mock.patch.object(
                managed_targets.shutil, "which", return_value="/bin/kache"
            ),
            mock.patch.object(
                managed_targets.subprocess,
                "run",
                return_value=mock.Mock(returncode=0, stdout=status, stderr=""),
            ),
        ):
            with self.assertRaisesRegex(ManagedTargetsError, "client version"):
                prewarm_kache({"kache_gc": True})

    def test_kache_gc_runs_only_after_real_deletion(self) -> None:
        with (
            mock.patch.object(
                managed_targets.shutil, "which", return_value="/bin/kache"
            ),
            mock.patch.object(managed_targets.subprocess, "run") as run,
        ):
            run_kache_gc({"max_age_days": 14}, {"actions": [{"status": "skipped"}]})
            run.assert_not_called()

            run.return_value = subprocess.CompletedProcess(
                ["kache"], 0, '{"removed":1}', ""
            )
            report: dict[str, object] = {
                "actions": [{"id": "target-a", "status": "deleted"}]
            }
            self.assertTrue(run_kache_gc({"max_age_days": 14}, report))
        self.assertEqual(report["kache_gc"], {"removed": 1})
        gc_call = run.call_args
        self.assertIsNotNone(gc_call)
        self.assertEqual(gc_call.args[0], ["/bin/kache", "gc", "--json"])

    def test_kache_gc_failure_keeps_deletion_receipt(self) -> None:
        with (
            mock.patch.object(
                managed_targets.shutil, "which", return_value="/bin/kache"
            ),
            mock.patch.object(
                managed_targets.subprocess,
                "run",
                return_value=subprocess.CompletedProcess(["kache"], 1, "", "busy"),
            ),
        ):
            report: dict[str, object] = {
                "actions": [{"id": "target-a", "status": "deleted"}]
            }
            self.assertFalse(run_kache_gc({"max_age_days": 14}, report))
        actions = report["actions"]
        if not isinstance(actions, list) or not actions:
            self.fail("Kache GC failure report lost the deletion action")
        action = actions[0]
        if not isinstance(action, dict):
            self.fail("Kache GC failure report has an invalid action")
        self.assertEqual(action["status"], "deleted")
        kache_report = report["kache_gc"]
        if not isinstance(kache_report, dict):
            self.fail("Kache GC failure report has no failure details")
        self.assertEqual(kache_report["status"], "failed")


if __name__ == "__main__":
    unittest.main()
