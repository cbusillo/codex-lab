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


if __name__ == "__main__":
    unittest.main()
