import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock


MODULE_PATH = Path(__file__).parents[1] / "just-shell.py"
SPEC = importlib.util.spec_from_file_location("just_shell", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {MODULE_PATH}")
just_shell = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(just_shell)


class JustShellTest(unittest.TestCase):
    def test_cargo_recipe_resolves_persistent_target(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            completed = mock.Mock(returncode=0, stdout=f"{root / 'target'}\n")
            with mock.patch("subprocess.run", return_value=completed) as run:
                environment = just_shell.resolve_cargo_environment(
                    "test", {"CODEX_REPO_ROOT": str(root)}
                )

        self.assertEqual(environment, {"CARGO_TARGET_DIR": str(root / "target")})
        run.assert_called_once_with(
            [str(root / "scripts" / "local" / "cargo-build-env.sh")],
            check=False,
            text=True,
            stdout=just_shell.subprocess.PIPE,
            stderr=None,
            env={"CODEX_REPO_ROOT": str(root)},
        )

    def test_non_cargo_recipe_does_not_resolve_target(self) -> None:
        with mock.patch("subprocess.run") as run:
            environment = just_shell.resolve_cargo_environment(
                "fmt", {"CODEX_REPO_ROOT": "/repo"}
            )

        self.assertEqual(environment, {})
        run.assert_not_called()

    def test_all_direct_cargo_recipes_resolve_target(self) -> None:
        expected = {
            "app-server-test-client",
            "bench",
            "clippy",
            "code-mode-host",
            "codex",
            "exec",
            "file-search",
            "fix",
            "install",
            "log",
            "mcp-server-run",
            "test",
            "tui-with-exec-server",
            "write-config-schema",
            "write-hooks-schema",
        }

        self.assertLessEqual(expected, just_shell.CARGO_ENV_RECIPES)

    def test_v8_environment_preserves_explicit_overrides(self) -> None:
        with mock.patch("subprocess.run") as run:
            environment = just_shell.resolve_rusty_v8_environment(
                "test",
                {
                    "CODEX_REPO_ROOT": "/repo",
                    "RUSTY_V8_ARCHIVE": "/cache/archive",
                    "RUSTY_V8_SRC_BINDING_PATH": "/cache/binding",
                },
            )

        self.assertEqual(environment, {})
        run.assert_not_called()

    def test_v8_environment_rejects_partial_override(self) -> None:
        with self.assertRaisesRegex(SystemExit, "2"):
            just_shell.resolve_rusty_v8_environment(
                "test",
                {"CODEX_REPO_ROOT": "/repo", "RUSTY_V8_ARCHIVE": "/cache/archive"},
            )

    @staticmethod
    def test_codex_core_tests_build_runtime_binaries() -> None:
        completed = mock.Mock(returncode=0)
        environment = {"CARGO_TARGET_DIR": "/tmp/target"}
        with mock.patch("subprocess.run", return_value=completed) as run:
            just_shell.build_test_prerequisites(
                "test", ["-p", "codex-core"], environment
            )

        run.assert_called_once_with(
            [
                "cargo",
                "build",
                "-p",
                "codex-cli",
                "--bin",
                "codex",
                "-p",
                "codex-code-mode-host",
                "--bin",
                "codex-code-mode-host",
                "-p",
                "codex-exec",
                "--bin",
                "codex-exec",
                "-p",
                "codex-rmcp-client",
                "--bin",
                "test_stdio_server",
                "-p",
                "codex-rmcp-client",
                "--bin",
                "test_streamable_http_server",
                "-p",
                "codex-shell-escalation",
                "--bin",
                "codex-execve-wrapper",
            ],
            check=False,
            env=environment,
        )

    @staticmethod
    def test_codex_core_lib_tests_skip_runtime_binaries() -> None:
        with mock.patch("subprocess.run") as run:
            just_shell.build_test_prerequisites(
                "test", ["--package=codex-core", "--lib"], {}
            )

        run.assert_not_called()

    @staticmethod
    def test_powershell_builds_test_prerequisites() -> None:
        completed = mock.Mock(returncode=0)
        recipe_args = ["-p", "codex-core"]
        with (
            mock.patch("shutil.which", return_value="pwsh.exe"),
            mock.patch("subprocess.run", return_value=completed) as run,
        ):
            exit_code = just_shell.run_powershell("cargo test", "test", recipe_args)

        assert exit_code == 0
        assert run.call_count == 2
        assert run.call_args_list[0].args[0][:2] == ["cargo", "build"]
        assert run.call_args_list[1].args[0][0] == "pwsh.exe"

    @staticmethod
    def test_other_package_tests_skip_codex_core_runtime_binaries() -> None:
        with mock.patch("subprocess.run") as run:
            just_shell.build_test_prerequisites(
                "test", ["-p", "codex-rollout-trace"], {}
            )

        run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
