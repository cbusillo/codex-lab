#!/usr/bin/env python3

from pathlib import Path
from collections.abc import Mapping
import json
import os
import subprocess
import tempfile
import unittest
from unittest import mock

from codex_lab_package.code_route import CodeRouteEngine
from codex_lab_package.code_route import CodeRouteRecoveryError
from codex_lab_package.code_route import LauncherTools
from codex_lab_package.code_route import activate_code_route
from codex_lab_package.code_route import deactivate_code_route
from codex_lab_package.code_route import read_code_route_state
from codex_lab_package.code_route import recover_code_route_transaction
from codex_lab_package.code_route import require_active_code_route
from codex_lab_package.code_route import sha256_file
from codex_lab_package.code_route_transaction import journal_path_for_state
import codex_lab_package.code_route_transaction as code_route_transaction


SOURCE_COMMIT = "a" * 40
SIGNING_IDENTIFIER = "com.shinycomputers.codex-lab.engine"
TEAM_IDENTIFIER = "MM5YXC7T6E"


class CodeRouteTest(unittest.TestCase):
    def test_regular_route_is_captured_launched_and_restored_exactly(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            state_path = write_state(root)
            engine, tools = write_engine_fixture(root)
            route_path = root / "bin" / "code"
            route_path.parent.mkdir()
            route_path.write_bytes(b"prior regular route\n")
            route_path.chmod(0o751)

            result = activate_code_route(
                state_path,
                engine,
                active_path=route_path,
                tools=tools,
            )

            self.assertTrue(result.changed)
            state = json.loads(state_path.read_text(encoding="utf-8"))
            self.assertEqual(state["sentinel"], "preserved")
            recorded = read_code_route_state(state, state_path)
            assert recorded is not None
            self.assertEqual(recorded.prior.kind, "regular")
            completed = subprocess.run(
                [str(route_path), "hello", "world"],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(
                completed.stdout,
                f"{engine.lab_home}|{engine.lab_home}|hello world\n",
            )

            deactivated = deactivate_code_route(state_path, active_path=route_path)

            self.assertTrue(deactivated.restored_prior)
            self.assertEqual(route_path.read_bytes(), b"prior regular route\n")
            self.assertEqual(route_path.stat().st_mode & 0o777, 0o751)
            self.assertIsNone(
                json.loads(state_path.read_text(encoding="utf-8"))["codeRoute"]
            )

    def test_dangling_symlink_route_is_captured_and_restored(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            state_path = write_state(root)
            engine, tools = write_engine_fixture(root)
            route_path = root / "bin" / "code"
            route_path.parent.mkdir()
            route_path.symlink_to("../missing-code")

            activate_code_route(
                state_path,
                engine,
                active_path=route_path,
                tools=tools,
            )
            state = json.loads(state_path.read_text(encoding="utf-8"))
            recorded = read_code_route_state(state, state_path)
            assert recorded is not None
            self.assertEqual(recorded.prior.kind, "symlink")
            self.assertEqual(recorded.prior.symlink_target, "../missing-code")

            deactivate_code_route(state_path, active_path=route_path)

            self.assertTrue(route_path.is_symlink())
            self.assertEqual(os.readlink(route_path), "../missing-code")

    def test_launcher_fails_closed_on_engine_digest_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            state_path = write_state(root)
            engine, tools = write_engine_fixture(root)
            route_path = root / "bin" / "code"
            activate_code_route(
                state_path,
                engine,
                active_path=route_path,
                tools=tools,
            )
            with engine.path.open("a", encoding="utf-8") as handle:
                handle.write("\n# changed\n")

            completed = subprocess.run(
                [str(route_path)],
                capture_output=True,
                text=True,
            )

            self.assertEqual(completed.returncode, 1)
            self.assertIn("SHA-256 does not match", completed.stderr)

    def test_launcher_fails_closed_on_provenance_mismatch(self) -> None:
        cases = {
            "source commit": {"source_commit": "b" * 40},
            "release identity": {"release_version": "1.2.3-lab.9"},
            "not clean": {"dirty_state": "dirty"},
            "provenance path": {"executable_path": "/tmp/not-the-engine"},
        }
        for message, overrides in cases.items():
            with (
                self.subTest(message=message),
                tempfile.TemporaryDirectory() as temp_dir,
            ):
                root = Path(temp_dir).resolve()
                state_path = write_state(root)
                engine, tools = write_engine_fixture(
                    root,
                    provenance_overrides=overrides,
                )
                route_path = root / "bin" / "code"
                activate_code_route(
                    state_path,
                    engine,
                    active_path=route_path,
                    tools=tools,
                )

                completed = subprocess.run(
                    [str(route_path)],
                    capture_output=True,
                    text=True,
                )

                self.assertEqual(completed.returncode, 1)
                self.assertIn(message, completed.stderr)

    def test_activation_rollback_restores_prior_route(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            state_path = write_state(root)
            engine, tools = write_engine_fixture(root)
            route_path = root / "bin" / "code"
            route_path.parent.mkdir()
            route_path.write_text("prior", encoding="utf-8")

            with mock.patch(
                "codex_lab_package.code_route.write_state_code_route",
                side_effect=OSError("state write failed"),
            ):
                with self.assertRaisesRegex(OSError, "state write failed"):
                    activate_code_route(
                        state_path,
                        engine,
                        active_path=route_path,
                        tools=tools,
                    )

            self.assertEqual(route_path.read_text(encoding="utf-8"), "prior")
            self.assertEqual(
                json.loads(state_path.read_text(encoding="utf-8")),
                {"codeRoute": None, "sentinel": "preserved"},
            )

    def test_recovers_interrupted_activation_before_state_commit(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            state_path = write_state(root)
            engine, tools = write_engine_fixture(root)
            route_path = root / "bin" / "code"
            route_path.parent.mkdir()
            route_path.write_text("prior", encoding="utf-8")
            original_safe_rename = code_route_transaction.safe_rename
            rename_count = 0

            def interrupt_after_rename(source: Path, target: Path) -> None:
                nonlocal rename_count
                original_safe_rename(source, target)
                rename_count += 1
                if rename_count == 2:
                    raise KeyboardInterrupt

            with mock.patch.object(
                code_route_transaction,
                "safe_rename",
                side_effect=interrupt_after_rename,
            ):
                with self.assertRaises(KeyboardInterrupt):
                    activate_code_route(
                        state_path,
                        engine,
                        active_path=route_path,
                        tools=tools,
                    )

            self.assertTrue(journal_path_for_state(state_path).is_file())
            recover_code_route_transaction(state_path, active_path=route_path)

            self.assertEqual(route_path.read_text(encoding="utf-8"), "prior")
            self.assertFalse(journal_path_for_state(state_path).exists())
            self.assertIsNone(
                json.loads(state_path.read_text(encoding="utf-8"))["codeRoute"]
            )

    def test_recovers_committed_activation_with_pending_journal(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            state_path = write_state(root)
            engine, tools = write_engine_fixture(root)
            route_path = root / "bin" / "code"

            with mock.patch.object(
                code_route_transaction,
                "clear_journal",
                side_effect=KeyboardInterrupt,
            ):
                with self.assertRaises(KeyboardInterrupt):
                    activate_code_route(
                        state_path,
                        engine,
                        active_path=route_path,
                        tools=tools,
                    )

            self.assertTrue(journal_path_for_state(state_path).is_file())
            recover_code_route_transaction(state_path, active_path=route_path)

            state = json.loads(state_path.read_text(encoding="utf-8"))
            route = read_code_route_state(state, state_path)
            assert route is not None
            require_active_code_route(route, expected_path=route_path)
            self.assertFalse(journal_path_for_state(state_path).exists())

    def test_recovery_rejects_journal_for_different_active_path(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            state_path = write_state(root)
            engine, tools = write_engine_fixture(root)
            route_path = root / "bin" / "code"
            victim_path = root / "do-not-delete"
            victim_path.write_text("preserved", encoding="utf-8")

            with mock.patch.object(
                code_route_transaction,
                "clear_journal",
                side_effect=KeyboardInterrupt,
            ):
                with self.assertRaises(KeyboardInterrupt):
                    activate_code_route(
                        state_path,
                        engine,
                        active_path=route_path,
                        tools=tools,
                    )

            journal_path = journal_path_for_state(state_path)
            journal = json.loads(journal_path.read_text(encoding="utf-8"))
            journal["activePath"] = str(victim_path)
            journal_path.write_text(json.dumps(journal), encoding="utf-8")
            journal_path.chmod(0o600)

            with self.assertRaisesRegex(
                CodeRouteRecoveryError, "different active path"
            ):
                recover_code_route_transaction(state_path, active_path=route_path)

            self.assertEqual(victim_path.read_text(encoding="utf-8"), "preserved")
            self.assertTrue(journal_path.exists())

    def test_activation_cas_uses_exact_state_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            state_path = write_state(root)
            engine, tools = write_engine_fixture(root)
            route_path = root / "bin" / "code"
            original_read = (
                code_route_transaction.code_route.read_state_document_with_sha256
            )

            def read_then_change_state(path: Path) -> tuple[dict, str]:
                state, digest = original_read(path)
                changed_state = dict(state)
                changed_state["concurrent"] = "preserved"
                path.write_text(
                    json.dumps(changed_state, indent=2, sort_keys=True) + "\n",
                    encoding="utf-8",
                )
                return state, digest

            with (
                mock.patch.object(
                    code_route_transaction.code_route,
                    "read_state_document_with_sha256",
                    side_effect=read_then_change_state,
                ),
                self.assertRaisesRegex(CodeRouteRecoveryError, "ambiguous"),
            ):
                activate_code_route(
                    state_path,
                    engine,
                    active_path=route_path,
                    tools=tools,
                )

            state = json.loads(state_path.read_text(encoding="utf-8"))
            self.assertEqual(state["concurrent"], "preserved")
            self.assertIsNone(state["codeRoute"])
            self.assertTrue(journal_path_for_state(state_path).exists())

    def test_rejects_unsafe_state_parent_without_touching_route(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            real_state_parent = root / "real-state"
            real_state_parent.mkdir()
            state_path = write_state(real_state_parent)
            linked_state_parent = root / "linked-state"
            linked_state_parent.symlink_to(real_state_parent, target_is_directory=True)
            engine, tools = write_engine_fixture(root)
            route_path = root / "bin" / "code"

            with self.assertRaisesRegex(ValueError, "parent path is unsafe"):
                activate_code_route(
                    linked_state_parent / "state" / state_path.name,
                    engine,
                    active_path=route_path,
                    tools=tools,
                )

            self.assertFalse(route_path.exists())

    def test_rejects_unsafe_route_paths_and_tampered_launcher(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            state_path = write_state(root)
            engine, tools = write_engine_fixture(root)
            directory_route = root / "directory-code"
            directory_route.mkdir()
            with self.assertRaisesRegex(ValueError, "absent, a symlink, or a regular"):
                activate_code_route(
                    state_path,
                    engine,
                    active_path=directory_route,
                    tools=tools,
                )

            real_parent = root / "real-bin"
            real_parent.mkdir()
            linked_parent = root / "linked-bin"
            linked_parent.symlink_to(real_parent, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "parent path is unsafe"):
                activate_code_route(
                    state_path,
                    engine,
                    active_path=linked_parent / "code",
                    tools=tools,
                )

            route_path = root / "bin" / "code"
            activate_code_route(
                state_path,
                engine,
                active_path=route_path,
                tools=tools,
            )
            route_path.write_text("tampered", encoding="utf-8")
            recorded = read_code_route_state(
                json.loads(state_path.read_text(encoding="utf-8")),
                state_path,
            )
            assert recorded is not None
            with self.assertRaisesRegex(ValueError, "launcher has changed"):
                require_active_code_route(recorded, expected_path=route_path)


def write_state(root: Path) -> Path:
    state_path = root / "state" / "install-state.json"
    state_path.parent.mkdir()
    state_path.write_text(
        json.dumps({"codeRoute": None, "sentinel": "preserved"}),
        encoding="utf-8",
    )
    return state_path


def write_engine_fixture(
    root: Path,
    *,
    provenance_overrides: Mapping[str, object] | None = None,
) -> tuple[CodeRouteEngine, LauncherTools]:
    engine_path = root / "lab-home" / "packages" / "standalone" / "current" / "codex"
    engine_path.parent.mkdir(parents=True)
    provenance: dict[str, object] = {
        "schema_version": 2,
        "version": "1.2.3",
        "release_version": "1.2.3-lab.1",
        "compatibility_version": "1.2.3",
        "source_commit": SOURCE_COMMIT,
        "dirty_state": "clean",
        "build_profile": "release",
        "build_channel": "release",
        "executable_path": str(engine_path),
    }
    provenance.update(provenance_overrides or {})
    engine_path.write_text(
        "#!/bin/sh\n"
        'if [ "${1:-}" = debug ] && [ "${2:-}" = provenance ]; then\n'
        f"  cat <<'EOF'\n{json.dumps(provenance)}\nEOF\n"
        "  exit 0\n"
        "fi\n"
        'printf "%s|%s|%s\\n" "$CODEX_HOME" "$CODEX_LAB_HOME" "$*"\n',
        encoding="utf-8",
    )
    engine_path.chmod(0o755)
    codesign_path = root / "fake-codesign"
    codesign_path.write_text(
        "#!/bin/sh\n"
        'if [ "${1:-}" = --verify ]; then exit 0; fi\n'
        f"echo 'Identifier={SIGNING_IDENTIFIER}' >&2\n"
        f"echo 'TeamIdentifier={TEAM_IDENTIFIER}' >&2\n",
        encoding="utf-8",
    )
    codesign_path.chmod(0o755)
    return (
        CodeRouteEngine(
            path=engine_path,
            sha256=sha256_file(engine_path),
            signing_identifier=SIGNING_IDENTIFIER,
            source_commit=SOURCE_COMMIT,
            team_identifier=TEAM_IDENTIFIER,
            release_tag="codex-lab-v1.2.3-lab.1",
            release_version="1.2.3-lab.1",
            version="1.2.3",
            build_channel="release",
            lab_home=root / "lab-home",
        ),
        LauncherTools(codesign=codesign_path),
    )


if __name__ == "__main__":
    unittest.main()
