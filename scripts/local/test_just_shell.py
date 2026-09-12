import importlib.util
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).parents[1]))

from local.target_lease import TARGET_LEASE_FD_ENV


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

    def test_cargo_recipe_does_not_launch_when_resolver_fails(self) -> None:
        completed = mock.Mock(returncode=1, stdout="")
        with (
            mock.patch.dict(os.environ, {"CODEX_REPO_ROOT": "/repo"}),
            mock.patch("subprocess.run", return_value=completed),
            mock.patch("os.execvp") as execvp,
            self.assertRaises(SystemExit) as raised,
        ):
            just_shell.run_sh("printf launched", "codex", [])

        self.assertEqual(raised.exception.code, 1)
        execvp.assert_not_called()

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

    def test_codex_core_prerequisite_cargo_inherits_target_lease(self) -> None:
        if os.name == "nt":
            self.skipTest("POSIX lease descriptors are unavailable on Windows")
        import fcntl

        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            lock_path = root / "lease.lock"
            fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o600)
            os.set_inheritable(fd, True)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            cargo = root / "cargo"
            cargo.write_text(
                "#!/usr/bin/env python3\n"
                "import fcntl, os\n"
                "from pathlib import Path\n"
                f"fd = int(os.environ[{TARGET_LEASE_FD_ENV!r}])\n"
                "assert os.get_inheritable(fd)\n"
                "fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)\n"
                f"Path({str(root / 'result')!r}).write_text('forwarded')\n",
                encoding="utf-8",
            )
            cargo.chmod(cargo.stat().st_mode | stat.S_IXUSR)
            environment = {
                "PATH": f"{root}{os.pathsep}{os.environ['PATH']}",
                "CARGO_TARGET_DIR": str(root / "target"),
                TARGET_LEASE_FD_ENV: str(fd),
            }
            try:
                just_shell.build_test_prerequisites(
                    "test", ["-p", "codex-core"], environment
                )
            finally:
                fcntl.flock(fd, fcntl.LOCK_UN)
                os.close(fd)
            self.assertEqual((root / "result").read_text(), "forwarded")

    def test_invalid_target_lease_fails_before_prerequisite_spawn(self) -> None:
        if os.name == "nt":
            self.skipTest("POSIX lease descriptors are unavailable on Windows")
        environment = {
            "CARGO_TARGET_DIR": "/tmp/target",
            TARGET_LEASE_FD_ENV: "not-a-fd",
        }
        with mock.patch("subprocess.run") as run:
            with self.assertRaisesRegex(ValueError, TARGET_LEASE_FD_ENV):
                just_shell.build_test_prerequisites(
                    "test", ["-p", "codex-core"], environment
                )
        run.assert_not_called()

    def test_stripped_target_lease_refuses_before_cargo_spawn(self) -> None:
        if os.name == "nt":
            self.skipTest("POSIX target leases are not supported on Windows")
        with tempfile.TemporaryFile() as lease, mock.patch("subprocess.run") as run:
            os.set_inheritable(lease.fileno(), True)
            with mock.patch.dict(
                os.environ, {TARGET_LEASE_FD_ENV: str(lease.fileno())}
            ):
                with self.assertRaisesRegex(ValueError, "stripped"):
                    just_shell.build_test_prerequisites(
                        "test", ["-p", "codex-core"], {}
                    )
            run.assert_not_called()

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

    def test_v8_resolver_inherits_target_lease(self) -> None:
        if os.name == "nt":
            self.skipTest("POSIX lease descriptors are unavailable on Windows")
        import fcntl

        with tempfile.TemporaryDirectory(dir=Path.cwd()) as temporary_directory:
            root = Path(temporary_directory)
            helper = root / "scripts" / "local" / "rusty_v8_env.py"
            helper.parent.mkdir(parents=True)
            marker = root / "forwarded"
            helper.write_text(
                "import fcntl, os\n"
                "from pathlib import Path\n"
                f"fd = int(os.environ[{TARGET_LEASE_FD_ENV!r}])\n"
                "assert os.get_inheritable(fd)\n"
                "fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)\n"
                f"Path({str(marker)!r}).write_text('forwarded')\n"
                "print('RUSTY_V8_ARCHIVE=/cache/archive')\n"
                "print('RUSTY_V8_SRC_BINDING_PATH=/cache/binding')\n",
                encoding="utf-8",
            )
            lock_path = root / "lease.lock"
            fd = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o600)
            os.set_inheritable(fd, True)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            try:
                result = just_shell.resolve_rusty_v8_environment(
                    "test",
                    {"CODEX_REPO_ROOT": str(root), TARGET_LEASE_FD_ENV: str(fd)},
                )
            finally:
                fcntl.flock(fd, fcntl.LOCK_UN)
                os.close(fd)
            self.assertEqual(
                result,
                {
                    "RUSTY_V8_ARCHIVE": "/cache/archive",
                    "RUSTY_V8_SRC_BINDING_PATH": "/cache/binding",
                },
            )
            self.assertEqual(marker.read_text(), "forwarded")



if __name__ == "__main__":
    unittest.main()
