from pathlib import Path
import os
import stat
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("configure-cached-bazel.sh")
RUNNER = SCRIPT.with_name("run_bazel_with_buildbuddy.py")


class ConfigureCachedBazelTests(unittest.TestCase):
    def make_fixture(self, root: Path, version: str) -> tuple[Path, Path]:
        repo = root / "repo"
        repo.mkdir()
        (repo / ".bazelversion").write_text("9.0.0\n", encoding="utf-8")
        cache = root / "bazelisk"
        binary = cache / "downloads" / "sha256" / "fixture" / "bin" / "bazel"
        binary.parent.mkdir(parents=True)
        binary.write_text(
            "#!/bin/sh\n"
            'if [ "$1" = "--ignore_all_rc_files" ]; then\n'
            '  printf "%s\\n" "$*" >> "$BAZEL_VERSION_LOG"\n'
            f'  printf "bazel {version}\\n"\n'
            "else\n"
            '  for arg in "$@"; do printf "<%s>\\n" "$arg" >> "$BAZEL_ARGS_LOG"; done\n'
            "fi\n",
            encoding="utf-8",
        )
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
        return repo, cache

    def run_script(self, root: Path, repo: Path, cache: Path, **extra: str):
        env = os.environ.copy()
        env.update(
            {
                "GITHUB_WORKSPACE": str(repo),
                "BAZELISK_HOME": str(cache),
                "CI_BUILD_ROOT": str(root / "ci"),
                "BAZEL_OUTPUT_BASE": str(root / "ci" / "o"),
                "GITHUB_ENV": str(root / "github-env"),
                "GITHUB_PATH": str(root / "github-path"),
                "BAZEL_VERSION_LOG": str(root / "version-log"),
                "BAZEL_ARGS_LOG": str(root / "bazel-args-log"),
                **extra,
            }
        )
        return subprocess.run(
            [str(SCRIPT)],
            cwd=repo,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_selects_matching_cached_binary_without_touching_home(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "fixture root"
            root.mkdir()
            repo, cache = self.make_fixture(root, "9.0.0")
            home = root / "home"
            home.mkdir()
            home_rc = home / ".bazelrc"
            home_rc.write_text("startup --output_base=/shared\n", encoding="utf-8")
            existing_rc = root / "existing.rc"
            existing_rc.write_text("build --show_timestamps\n", encoding="utf-8")

            completed = self.run_script(
                root,
                repo,
                cache,
                HOME=str(home),
                BAZELRC=str(existing_rc),
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(
                home_rc.read_text(encoding="utf-8"), "startup --output_base=/shared\n"
            )
            job_rc = root / "ci" / "bazelrc"
            self.assertIn(
                "startup --output_base='" + str(root / "ci" / "o") + "'",
                job_rc.read_text(),
            )
            env_lines = (root / "github-env").read_text(encoding="utf-8").splitlines()
            self.assertIn(f"BAZELRC={existing_rc},{job_rc}", env_lines)
            self.assertIn(f"CODEX_BAZEL_BIN={root / 'ci' / 'bin' / 'bazel'}", env_lines)
            self.assertIn(str(root / "ci" / "bin"), (root / "github-path").read_text())
            self.assertEqual(
                (root / "version-log").read_text(encoding="utf-8"),
                "--ignore_all_rc_files --version\n",
            )
            shim = root / "ci" / "bin" / "bazel"
            shim_env = os.environ.copy()
            shim_env.update(
                {
                    "BAZEL_VERSION_LOG": str(root / "version-log-2"),
                    "BAZEL_ARGS_LOG": str(root / "bazel-args-log"),
                }
            )
            invoked = subprocess.run(
                [str(shim), "build", "//target", "argument with spaces"],
                env=shim_env,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(invoked.returncode, 0, invoked.stderr)
            self.assertEqual(
                (root / "bazel-args-log").read_text(encoding="utf-8").splitlines(),
                ["<build>", "<//target>", "<argument with spaces>"],
            )

            import sys

            sys.path.insert(0, str(RUNNER.parent))
            import run_bazel_with_buildbuddy

            self.assertEqual(
                run_bazel_with_buildbuddy.bazel_command(
                    "build", env={"CODEX_BAZEL_BIN": str(shim)}
                ),
                [str(shim), "build"],
            )

    def test_mismatch_fails_closed_before_writing_job_configuration(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo, cache = self.make_fixture(root, "8.4.0")

            completed = self.run_script(root, repo, cache, HOME=str(root / "home"))

            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("no cached binary matches Bazel 9.0.0", completed.stderr)
            self.assertFalse((root / "ci").exists())
            self.assertFalse((root / "github-env").exists())
            self.assertFalse((root / "github-path").exists())

    def test_missing_cache_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repo = root / "repo"
            repo.mkdir()
            (repo / ".bazelversion").write_text("9.0.0\n", encoding="utf-8")

            completed = self.run_script(
                root, repo, root / "missing", HOME=str(root / "home")
            )

            self.assertNotEqual(completed.returncode, 0)
            self.assertIn("no cached binary matches Bazel 9.0.0", completed.stderr)


if __name__ == "__main__":
    unittest.main()
