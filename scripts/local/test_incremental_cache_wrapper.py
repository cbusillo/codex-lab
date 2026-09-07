import importlib.util
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock


MODULE_PATH = Path(__file__).with_name("incremental-cache-wrapper.py")
SPEC = importlib.util.spec_from_file_location("incremental_cache_wrapper", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {MODULE_PATH}")
wrapper: Any = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wrapper)


def mock_executable(directory: Path, name: str, log: Path) -> Path:
    executable = directory / name
    executable.write_text(
        "#!" + sys.executable + "\n"
        "import json, os, sys\n"
        f"path = {str(log)!r}\n"
        "with open(path, 'w', encoding='utf-8') as output:\n"
        "    json.dump({'args': sys.argv[1:], 'marker': os.environ.get('PILOT_MARKER'),\n"
        "        'incremental': os.environ.get('CARGO_INCREMENTAL')}, output)\n",
        encoding="utf-8",
    )
    executable.chmod(executable.stat().st_mode | stat.S_IXUSR)
    return executable


def pilot_environment(**updates: str) -> dict[str, str]:
    environment = os.environ.copy()
    environment.pop("RUSTC_WORKSPACE_WRAPPER", None)
    environment.update(updates)
    return environment


def run_wrapper(
    arguments: list[str], **environment: str
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(MODULE_PATH), *arguments],
        env=pilot_environment(**environment),
        check=False,
        capture_output=True,
        text=True,
    )


def read_log(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


@unittest.skipIf(os.name == "nt", "POSIX shebang executable pilot")
class IncrementalCacheWrapperTest(unittest.TestCase):
    def test_incremental_codegen_forms_are_detected(self) -> None:
        for arguments in (
            ["-C", "incremental=/tmp/cache"],
            ["-Cincremental=/tmp/cache"],
            ["--codegen", "incremental=/tmp/cache"],
            ["--codegen=incremental=/tmp/cache"],
        ):
            with self.subTest(arguments=arguments):
                self.assertTrue(wrapper.has_incremental_codegen(arguments))
        self.assertFalse(wrapper.has_incremental_codegen(["-C", "metadata=abc"]))
        self.assertFalse(wrapper.has_incremental_codegen(["--", "-C", "incremental=x"]))

    def test_incremental_call_executes_compiler_and_preserves_arguments(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            log = root / "compiler.json"
            compiler = mock_executable(root, "rustc-mock", log)
            environment = pilot_environment(
                PILOT_MARKER="preserved",
                CARGO_INCREMENTAL="1",
                **{wrapper.CACHE_EXECUTABLE_ENV: str(root / "missing-sccache")},
            )
            result = subprocess.run(
                [
                    sys.executable,
                    str(MODULE_PATH),
                    str(compiler),
                    "--crate-name",
                    "demo value",
                    "-Cincremental=/tmp/target",
                ],
                env=environment,
                check=False,
            )
            self.assertEqual(result.returncode, 0)
            self.assertEqual(
                json.loads(log.read_text(encoding="utf-8")),
                {
                    "args": ["--crate-name", "demo value", "-Cincremental=/tmp/target"],
                    "marker": "preserved",
                    "incremental": "1",
                },
            )

    def test_nonincremental_call_executes_configured_sccache(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            log = root / "sccache.json"
            compiler = root / "rustc-mock"
            cache = mock_executable(root, "sccache-mock", log)
            result = subprocess.run(
                [
                    sys.executable,
                    str(MODULE_PATH),
                    str(compiler),
                    "--crate-name",
                    "demo",
                ],
                env=pilot_environment(
                    PILOT_MARKER="preserved",
                    CARGO_INCREMENTAL="1",
                    **{wrapper.CACHE_EXECUTABLE_ENV: str(cache)},
                ),
                check=False,
            )
            self.assertEqual(result.returncode, 0)
            self.assertEqual(
                json.loads(log.read_text(encoding="utf-8")),
                {
                    "args": [str(compiler), "--crate-name", "demo"],
                    "marker": "preserved",
                    "incremental": None,
                },
            )

    def test_missing_cache_fails_visibly(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                str(MODULE_PATH),
                "/missing/rustc",
                "--crate-name",
                "demo",
            ],
            env=pilot_environment(**{wrapper.CACHE_EXECUTABLE_ENV: ""}),
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 127)
        self.assertIn("sccache is required", result.stderr)

    def test_nested_wrapper_fails_before_reentering_itself(self) -> None:
        result = subprocess.run(
            [sys.executable, str(MODULE_PATH), str(MODULE_PATH), "/missing/rustc"],
            env=pilot_environment(),
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("nested RUSTC_WRAPPER", result.stderr)

    def test_workspace_wrapper_configuration_is_rejected(self) -> None:
        result = subprocess.run(
            [sys.executable, str(MODULE_PATH), "/missing/rustc"],
            env=pilot_environment(RUSTC_WORKSPACE_WRAPPER="other-wrapper"),
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("RUSTC_WORKSPACE_WRAPPER is unsupported", result.stderr)

    def test_configured_cache_cannot_point_to_this_wrapper(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                str(MODULE_PATH),
                "/missing/rustc",
                "--crate-name",
                "demo",
            ],
            env=pilot_environment(**{wrapper.CACHE_EXECUTABLE_ENV: str(MODULE_PATH)}),
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("points to this wrapper", result.stderr)

    def test_response_file_executes_compiler_without_scanning(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            log = root / "compiler.json"
            compiler = mock_executable(root, "rustc-mock", log)
            result = subprocess.run(
                [sys.executable, str(MODULE_PATH), str(compiler), "@opaque.rsp"],
                env=pilot_environment(
                    **{wrapper.CACHE_EXECUTABLE_ENV: str(root / "missing-sccache")}
                ),
                check=False,
            )
            self.assertEqual(result.returncode, 0)
            self.assertEqual(
                json.loads(log.read_text(encoding="utf-8"))["args"], ["@opaque.rsp"]
            )

    def test_existing_sccache_is_not_wrapped_again(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            log = root / "sccache.json"
            sccache = mock_executable(root, "sccache", log)
            result = subprocess.run(
                [
                    sys.executable,
                    str(MODULE_PATH),
                    str(sccache),
                    "/missing/rustc",
                    "--crate-name",
                    "demo",
                ],
                env=pilot_environment(
                    CARGO_INCREMENTAL="1",
                    **{wrapper.CACHE_EXECUTABLE_ENV: ""},
                ),
                check=False,
            )
            self.assertEqual(result.returncode, 0)
            self.assertEqual(
                json.loads(log.read_text(encoding="utf-8"))["args"],
                ["/missing/rustc", "--crate-name", "demo"],
            )
            self.assertIsNone(
                json.loads(log.read_text(encoding="utf-8"))["incremental"]
            )

    def test_configured_cache_name_is_resolved_before_self_check(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            cache = mock_executable(root, "cache-wrapper", root / "cache.json")
            environment = pilot_environment(
                **{wrapper.CACHE_EXECUTABLE_ENV: cache.name},
                PATH=f"{root}{os.pathsep}{os.environ.get('PATH', '')}",
            )
            result = subprocess.run(
                [
                    sys.executable,
                    str(MODULE_PATH),
                    "/missing/rustc",
                    "--crate-name",
                    "demo",
                ],
                env=environment,
                check=False,
            )
            self.assertEqual(result.returncode, 0)
            self.assertEqual(
                read_log(root / "cache.json")["args"],
                ["/missing/rustc", "--crate-name", "demo"],
            )

    def test_incremental_existing_sccache_is_unwrapped(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            cache_log = root / "sccache.json"
            compiler_log = root / "compiler.json"
            sccache = mock_executable(root, "sccache", cache_log)
            compiler = mock_executable(root, "rustc-mock", compiler_log)
            result = subprocess.run(
                [
                    sys.executable,
                    str(MODULE_PATH),
                    str(sccache),
                    str(compiler),
                    "-Cincremental=/tmp/cache",
                ],
                env=pilot_environment(
                    CARGO_INCREMENTAL="1",
                    **{wrapper.CACHE_EXECUTABLE_ENV: ""},
                ),
                check=False,
            )
            self.assertEqual(result.returncode, 0)
            self.assertEqual(
                json.loads(compiler_log.read_text(encoding="utf-8"))["args"],
                ["-Cincremental=/tmp/cache"],
            )
            self.assertEqual(
                json.loads(compiler_log.read_text(encoding="utf-8"))["incremental"],
                "1",
            )
            self.assertFalse(cache_log.exists())


class PlatformGuardTest(unittest.TestCase):
    def test_windows_fails_closed(self) -> None:
        with mock.patch.object(wrapper.os, "name", "nt"):
            with mock.patch("builtins.print") as print_mock:
                self.assertEqual(wrapper.main(["wrapper", "rustc"]), 2)
        self.assertIn("unsupported on Windows", print_mock.call_args.args[0])


if __name__ == "__main__":
    unittest.main()
