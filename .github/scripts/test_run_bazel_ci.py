#!/usr/bin/env python3

import os
import subprocess
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory


REPO_ROOT = Path(__file__).resolve().parents[2]
RUN_BAZEL_CI = REPO_ROOT / ".github" / "scripts" / "run-bazel-ci.sh"


class RunBazelCiTest(unittest.TestCase):
    def test_keyless_windows_cross_compile_uses_gnullvm_host(self) -> None:
        with TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            fake_bazel = temp_path / "fake-bazel"
            fake_bazel.write_text(
                "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" >\"$FAKE_BAZEL_ARGS\"\n",
                encoding="utf-8",
            )
            fake_bazel.chmod(0o755)
            args_path = temp_path / "args"

            env = os.environ.copy()
            env.pop("BUILDBUDDY_API_KEY", None)
            env.update(
                {
                    "CODEX_BAZEL_BIN": str(fake_bazel),
                    "CODEX_BAZEL_WINDOWS_PATH": r"C:\Windows\System32",
                    "FAKE_BAZEL_ARGS": str(args_path),
                    "RUNNER_OS": "Windows",
                }
            )

            subprocess.run(
                [
                    str(RUN_BAZEL_CI),
                    "--windows-cross-compile",
                    "--",
                    "build",
                    "--",
                    "//codex-rs/version:version",
                ],
                cwd=REPO_ROOT,
                env=env,
                check=True,
                capture_output=True,
                text=True,
            )

            bazel_args = args_path.read_text(encoding="utf-8").splitlines()
            self.assertIn("--host_platform=//:local_windows", bazel_args)
            self.assertNotIn("--host_platform=//:local_windows_msvc", bazel_args)


if __name__ == "__main__":
    unittest.main()
