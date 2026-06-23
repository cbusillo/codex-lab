#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path
from unittest import mock

SCRIPT_PATH = Path(__file__).resolve().parents[1] / "just-shell.py"
SPEC = importlib.util.spec_from_file_location("just_shell", SCRIPT_PATH)
assert SPEC is not None
assert SPEC.loader is not None
just_shell = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(just_shell)


class JustShellTest(unittest.TestCase):
    def test_cargo_recipe_evaluates_cargo_build_env(self) -> None:
        with mock.patch.object(just_shell.os, "execvp") as execvp:
            just_shell.run_sh("cargo test {args}", "test", ["-p", "codex-core"])

        self.assertEqual("sh", execvp.call_args.args[0])
        command = execvp.call_args.args[1][2]
        self.assertIn("scripts/local/cargo-build-env.sh", command)
        self.assertIn("|| exit", command)
        self.assertIn('cargo test "$@"', command)
        self.assertEqual(
            ["test", "-p", "codex-core"],
            execvp.call_args.args[1][3:],
        )

    def test_cargo_recipe_preserves_multiple_forwarded_args(self) -> None:
        with mock.patch.object(just_shell.os, "execvp") as execvp:
            just_shell.run_sh(
                "cargo test {args}",
                "test",
                ["-p", "codex-core", "--lib"],
            )

        self.assertEqual(
            ["test", "-p", "codex-core", "--lib"],
            execvp.call_args.args[1][3:],
        )

    def test_app_server_recipe_uses_cargo_target_dir_binary(self) -> None:
        with mock.patch.object(just_shell.os, "execvp") as execvp:
            just_shell.run_sh(
                'codex_bin="${CARGO_TARGET_DIR:-./target}/debug/codex"',
                "app-server-test-client",
                [],
            )

        command = execvp.call_args.args[1][2]
        self.assertIn("scripts/local/cargo-build-env.sh", command)
        self.assertIn("${CARGO_TARGET_DIR:-./target}/debug/codex", command)

    def test_exec_harness_recipe_keeps_own_target_env(self) -> None:
        with mock.patch.object(just_shell.os, "execvp") as execvp:
            just_shell.run_sh("cargo build", "exec-harness-test", [])

        command = execvp.call_args.args[1][2]
        self.assertNotIn("scripts/local/cargo-build-env.sh", command)
        self.assertEqual("cargo build", command)

    def test_non_cargo_recipe_is_unchanged(self) -> None:
        with mock.patch.object(just_shell.os, "execvp") as execvp:
            just_shell.run_sh(
                "python3 scripts/local/speed-status.sh", "local-speed-status", []
            )

        command = execvp.call_args.args[1][2]
        self.assertNotIn("scripts/local/cargo-build-env.sh", command)
        self.assertEqual("python3 scripts/local/speed-status.sh", command)


if __name__ == "__main__":
    unittest.main()
