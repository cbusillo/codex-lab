#!/usr/bin/env python3

import json
import os
import selectors
import subprocess
import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from time import monotonic
from time import sleep

import local_build_resources


assert local_build_resources.__file__ is not None
SCRIPT = Path(local_build_resources.__file__)
RUN_BAZEL_CI = SCRIPT.with_name("run-bazel-ci.sh")


class LocalBuildResourcesTest(unittest.TestCase):
    @staticmethod
    def profile_env(temp_dir: str) -> dict[str, str]:
        return {
            "CARGO_BUILD_JOBS": "4",
            "CODEX_LOCAL_BUILD_JOBS": "4",
            "CODEX_LOCAL_BUILD_LOCK": str(Path(temp_dir) / "build.lock"),
            "CODEX_LOCAL_BUILD_MEMORY_MB": "24576",
            "GITHUB_ACTIONS": "true",
            "RUNNER_ENVIRONMENT": "self-hosted",
            "RUNNER_OS": "macOS",
        }

    def test_local_bazel_build_gets_bounded_scheduler_args(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = self.profile_env(temp_dir)

            self.assertEqual(
                local_build_resources.bazel_resource_args(
                    ["build", "--config=ci-macos", "//codex-rs/..."], env
                ),
                [
                    "--jobs=4",
                    "--local_resources=cpu=4",
                    "--local_resources=memory=24576",
                ],
            )

    def test_rbe_and_query_invocations_do_not_get_local_scheduler_args(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = self.profile_env(temp_dir)
            remote_env = {**env, "BUILDBUDDY_API_KEY": "token"}

            self.assertEqual(
                local_build_resources.bazel_resource_args(
                    ["build", "--config=ci-macos", "//codex-rs/..."], remote_env
                ),
                [],
            )
            self.assertEqual(
                local_build_resources.bazel_resource_args(
                    ["cquery", "--config=ci-macos", "//codex-rs/..."], env
                ),
                [],
            )

    def test_unset_profile_preserves_existing_behavior(self) -> None:
        self.assertIsNone(local_build_resources.LocalBuildResources.from_env({}))
        self.assertEqual(
            local_build_resources.bazel_resource_args(
                ["build", "--config=ci-macos", "//codex-rs/..."], {}
            ),
            [],
        )

    def test_invalid_profiles_are_rejected(self) -> None:
        with TemporaryDirectory() as temp_dir:
            valid = self.profile_env(temp_dir)
            invalid_profiles = (
                {**valid, "CODEX_LOCAL_BUILD_MEMORY_MB": "0"},
                {**valid, "CODEX_LOCAL_BUILD_LOCK": "relative.lock"},
                {**valid, "CARGO_BUILD_JOBS": "3"},
                {**valid, "RUNNER_ENVIRONMENT": "github-hosted"},
                {**valid, "CODEX_LOCAL_BUILD_JOBS": ""},
                {
                    name: value
                    for name, value in valid.items()
                    if name != "CODEX_LOCAL_BUILD_LOCK"
                },
            )

            for env in invalid_profiles:
                with (
                    self.subTest(env=env),
                    self.assertRaises(local_build_resources.ResourceProfileError),
                ):
                    local_build_resources.LocalBuildResources.from_env(env)

    def test_invalid_profile_fails_before_launching_a_subprocess(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = {
                **os.environ,
                **self.profile_env(temp_dir),
                "CODEX_LOCAL_BUILD_JOBS": "four",
            }

            result = subprocess.run(
                [sys.executable, str(SCRIPT), "exec", "--", "unused-command"],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn(
                "CODEX_LOCAL_BUILD_JOBS must be a positive base-10 integer",
                result.stderr,
            )

    @unittest.skipUnless(sys.platform == "darwin", "local Bazel lock is macOS-only")
    def test_run_bazel_ci_passes_local_resource_args_to_fake_bazel(self) -> None:
        with TemporaryDirectory() as temp_dir:
            capture = Path(temp_dir) / "args.json"
            fake_bazel = self.write_fake_bazel(temp_dir, capture)
            env = {
                **os.environ,
                **self.profile_env(temp_dir),
                "CODEX_BAZEL_BIN": str(fake_bazel),
            }
            env.pop("BUILDBUDDY_API_KEY", None)

            result = subprocess.run(
                [str(RUN_BAZEL_CI), "--", "build", "--", "//codex-rs/..."],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            args = json.loads(capture.read_text(encoding="utf-8"))
            self.assertIn("--jobs=4", args)
            self.assertIn("--local_resources=cpu=4", args)
            self.assertIn("--local_resources=memory=24576", args)
            separator = args.index("--")
            self.assertLess(args.index("--jobs=4"), separator)
            self.assertLess(args.index("--local_resources=cpu=4"), separator)
            self.assertLess(args.index("--local_resources=memory=24576"), separator)

    def test_run_bazel_ci_does_not_limit_fake_rbe_invocation(self) -> None:
        with TemporaryDirectory() as temp_dir:
            capture = Path(temp_dir) / "args.json"
            fake_bazel = self.write_fake_bazel(temp_dir, capture)
            env = {
                **os.environ,
                **self.profile_env(temp_dir),
                "BUILDBUDDY_API_KEY": "token",
                "CODEX_BAZEL_BIN": str(fake_bazel),
                "GITHUB_REPOSITORY": "cbusillo/codex-lab",
            }

            result = subprocess.run(
                [str(RUN_BAZEL_CI), "--", "build", "--", "//codex-rs/..."],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            args = json.loads(capture.read_text(encoding="utf-8"))
            self.assertIn("--config=buildbuddy-generic-rbe", args)
            self.assertFalse(any(arg.startswith("--local_") for arg in args))
            self.assertFalse(any(arg.startswith("--jobs=") for arg in args))
            self.assertNotIn("Waiting for shared Codex Lab", result.stderr)

    @unittest.skipUnless(sys.platform == "darwin", "advisory build lock is macOS-only")
    def test_lock_serializes_fake_children_and_releases_on_exit(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = {**os.environ, **self.profile_env(temp_dir)}
            first_started = Path(temp_dir) / "first-started"
            release_first = Path(temp_dir) / "release-first"
            second_started = Path(temp_dir) / "second-started"
            first = self.start_waiting_child(env, first_started, release_first)
            try:
                self.read_until(first, "Acquired shared Codex Lab local build lock")
                second = self.start_marker_child(env, second_started)
                try:
                    self.read_until(
                        second, "Waiting for shared Codex Lab local build lock"
                    )
                    self.assertFalse(second_started.exists())

                    release_first.touch()
                    self.assertEqual(first.wait(timeout=5), 0)
                    self.assertEqual(second.wait(timeout=5), 0)
                    first.communicate(timeout=1)
                    second.communicate(timeout=1)
                    self.assertTrue(second_started.exists())
                    self.assertTrue(Path(env["CODEX_LOCAL_BUILD_LOCK"]).exists())
                finally:
                    if second.poll() is None:
                        second.terminate()
                    second.communicate(timeout=5)
            finally:
                if first.poll() is None:
                    release_first.touch()
                first.communicate(timeout=5)

    @unittest.skipUnless(sys.platform == "darwin", "advisory build lock is macOS-only")
    def test_cancellation_is_forwarded_while_lock_stays_held(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = {**os.environ, **self.profile_env(temp_dir)}
            child_started = Path(temp_dir) / "child-started"
            child_signaled = Path(temp_dir) / "child-signaled"
            release_child = Path(temp_dir) / "release-child"
            contender_started = Path(temp_dir) / "contender-started"
            code = (
                "import os, signal\n"
                "from pathlib import Path\n"
                "from time import sleep\n"
                f"started=Path({str(child_started)!r})\n"
                f"signaled=Path({str(child_signaled)!r})\n"
                f"release=Path({str(release_child)!r})\n"
                "def stop(_signum, _frame):\n"
                "    signaled.touch()\n"
                "    while not release.exists(): sleep(0.01)\n"
                "    signal.signal(signal.SIGTERM, signal.SIG_DFL)\n"
                "    os.kill(os.getpid(), signal.SIGTERM)\n"
                "signal.signal(signal.SIGTERM, stop)\n"
                "started.touch()\n"
                "signal.pause()\n"
            )
            supervised = self.start_helper(env, [sys.executable, "-c", code])
            contender = None
            try:
                self.read_until(
                    supervised, "Acquired shared Codex Lab local build lock"
                )
                self.wait_for_path(child_started)
                supervised.terminate()
                self.wait_for_path(child_signaled)

                contender = self.start_marker_child(env, contender_started)
                self.read_until(
                    contender, "Waiting for shared Codex Lab local build lock"
                )
                self.assertFalse(contender_started.exists())

                release_child.touch()
                self.assertEqual(supervised.wait(timeout=5), 143)
                self.assertEqual(contender.wait(timeout=5), 0)
                supervised.communicate(timeout=1)
                contender.communicate(timeout=1)
                self.assertTrue(contender_started.exists())
            finally:
                release_child.touch()
                if supervised.poll() is None:
                    supervised.terminate()
                supervised.communicate(timeout=5)
                if contender is not None:
                    if contender.poll() is None:
                        contender.terminate()
                    contender.communicate(timeout=5)

    @unittest.skipUnless(sys.platform == "darwin", "advisory build lock is macOS-only")
    def test_lock_descriptor_is_not_inherited_by_fake_child(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = {**os.environ, **self.profile_env(temp_dir)}
            lock_path = Path(env["CODEX_LOCAL_BUILD_LOCK"])
            result_path = Path(temp_dir) / "inherited"
            code = (
                "import os, sys\n"
                "from pathlib import Path\n"
                "lock_stat=os.stat(sys.argv[1])\n"
                "inherited=False\n"
                "for fd in range(3, 256):\n"
                "    try: fd_stat=os.fstat(fd)\n"
                "    except OSError: continue\n"
                "    if (fd_stat.st_dev, fd_stat.st_ino) == "
                "(lock_stat.st_dev, lock_stat.st_ino): inherited=True\n"
                "Path(sys.argv[2]).write_text(str(inherited), encoding='utf-8')\n"
            )

            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "exec",
                    "--",
                    sys.executable,
                    "-c",
                    code,
                    str(lock_path),
                    str(result_path),
                ],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result_path.read_text(encoding="utf-8"), "False")

    @unittest.skipUnless(sys.platform == "darwin", "advisory build lock is macOS-only")
    def test_signal_during_spawn_is_forwarded_and_child_is_reaped(self) -> None:
        with TemporaryDirectory() as temp_dir:
            env = {**os.environ, **self.profile_env(temp_dir)}
            child_pid_path = Path(temp_dir) / "child-pid"
            contender_started = Path(temp_dir) / "contender-started"
            fixture = (
                "import os, signal, subprocess, sys\n"
                "from pathlib import Path\n"
                f"sys.path.insert(0, {str(SCRIPT.parent)!r})\n"
                "import local_build_resources as resources\n"
                "real_popen = subprocess.Popen\n"
                "def spawn_and_cancel(*args, **kwargs):\n"
                "    child = real_popen(*args, **kwargs)\n"
                f"    Path({str(child_pid_path)!r}).write_text("
                "str(child.pid), encoding='utf-8')\n"
                "    os.kill(os.getpid(), signal.SIGTERM)\n"
                "    return child\n"
                "resources.subprocess.Popen = spawn_and_cancel\n"
                "status = resources.run_with_optional_lock(\n"
                "    [sys.executable, '-c', 'import signal; signal.pause()'])\n"
                "raise SystemExit(status)\n"
            )

            result = subprocess.run(
                [sys.executable, "-c", fixture],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 143, result.stderr)
            child_pid = int(child_pid_path.read_text(encoding="utf-8"))
            with self.assertRaises(ProcessLookupError):
                os.kill(child_pid, 0)
            contender = self.start_marker_child(env, contender_started)
            self.assertEqual(contender.wait(timeout=5), 0)
            contender.communicate(timeout=1)
            self.assertTrue(contender_started.exists())

    @staticmethod
    def write_fake_bazel(temp_dir: str, capture: Path) -> Path:
        script = Path(temp_dir) / "fake-bazel"
        script.write_text(
            "#!/usr/bin/env python3\n"
            "import json, sys\n"
            "from pathlib import Path\n"
            f"Path({str(capture)!r}).write_text("
            "json.dumps(sys.argv[1:]), encoding='utf-8')\n",
            encoding="utf-8",
        )
        script.chmod(0o755)
        return script

    def start_waiting_child(
        self, env: dict[str, str], started: Path, release: Path
    ) -> subprocess.Popen[str]:
        code = (
            "from pathlib import Path; from time import sleep; "
            f"started=Path({str(started)!r}); release=Path({str(release)!r}); "
            "started.touch(); "
            "\nwhile not release.exists(): sleep(0.01)"
        )
        return self.start_helper(env, [sys.executable, "-c", code])

    def start_marker_child(
        self, env: dict[str, str], marker: Path
    ) -> subprocess.Popen[str]:
        code = f"from pathlib import Path; Path({str(marker)!r}).touch()"
        return self.start_helper(env, [sys.executable, "-c", code])

    @staticmethod
    def start_helper(env: dict[str, str], command: list[str]) -> subprocess.Popen[str]:
        return subprocess.Popen(
            [sys.executable, str(SCRIPT), "exec", "--", *command],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

    def read_until(self, process: subprocess.Popen[str], expected: str) -> None:
        deadline = monotonic() + 5
        output = b""
        if process.stderr is None:
            self.fail("stderr pipe is unavailable")
        stderr = process.stderr
        selector = selectors.DefaultSelector()
        selector.register(stderr, selectors.EVENT_READ)
        try:
            while True:
                remaining = deadline - monotonic()
                if remaining <= 0:
                    break
                if not selector.select(remaining):
                    break
                chunk = os.read(stderr.fileno(), 4096)
                if not chunk:
                    break
                output += chunk
                if expected.encode() in output:
                    return
        finally:
            selector.close()
        self.fail(
            f"missing {expected!r} in stderr: {output.decode(errors='replace')!r}"
        )

    def wait_for_path(self, path: Path) -> None:
        deadline = monotonic() + 5
        while monotonic() < deadline:
            if path.exists():
                return
            sleep(0.01)
        self.fail(f"path was not created: {path}")


if __name__ == "__main__":
    unittest.main()
