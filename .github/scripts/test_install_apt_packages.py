#!/usr/bin/env python3
"""Contract tests for conditional apt installs on shared runners.

The release and musl jobs run on persistent self-hosted runners as well as
ephemeral GitHub-hosted ones, so the interesting behavior is what the script
does *not* do: no apt run when nothing is missing, and no silent hang when sudo
would prompt for a password.
"""

import os
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github/scripts/install-apt-packages.sh"
MUSL_SCRIPT = ROOT / ".github/scripts/install-musl-build-tools.sh"
RUST_RELEASE = ROOT / ".github/workflows/rust-release.yml"


class InstallAptPackagesTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.root = Path(self.temp_dir.name)
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.command_log = self.root / "commands.log"

    def write_stub(self, name: str, body: str) -> None:
        stub = self.bin_dir / name
        stub.write_text(f"#!/usr/bin/env bash\nset -euo pipefail\n{body}\n")
        stub.chmod(0o755)

    def stub_environment(
        self,
        installed: list[str],
        sudo_is_passwordless: bool,
        is_root: bool = False,
    ) -> None:
        installed_list = " ".join(installed)
        self.write_stub(
            "dpkg-query",
            textwrap.dedent(
                f"""\
                package="${{@: -1}}"
                for installed in {installed_list or '""'}; do
                  if [[ "$package" == "$installed" ]]; then
                    printf 'installed'
                    exit 0
                  fi
                done
                printf 'unknown'
                exit 1
                """
            ),
        )
        self.write_stub("apt-get", f'echo "apt-get $*" >> "{self.command_log}"')
        self.write_stub("id", f"echo {0 if is_root else 1000}")
        if sudo_is_passwordless:
            self.write_stub(
                "sudo",
                textwrap.dedent(
                    f"""\
                    if [[ "${{1:-}}" == "-n" ]]; then
                      shift
                      exec "$@"
                    fi
                    echo "sudo $*" >> "{self.command_log}"
                    exec "$@"
                    """
                ),
            )
        else:
            self.write_stub("sudo", 'echo "sudo: a password is required" >&2\nexit 1')

    def run_script(self, *packages: str) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env["PATH"] = f"{self.bin_dir}{os.pathsep}{env['PATH']}"
        return subprocess.run(
            ["bash", str(SCRIPT), *packages], env=env, capture_output=True, text=True
        )

    def logged_commands(self) -> list[str]:
        if not self.command_log.exists():
            return []
        return self.command_log.read_text().splitlines()

    def test_skips_apt_entirely_when_nothing_is_missing(self) -> None:
        self.stub_environment(
            installed=["binutils", "pkg-config"], sudo_is_passwordless=True
        )

        result = self.run_script("binutils", "pkg-config")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(self.logged_commands(), [])
        self.assertIn("already installed", result.stdout)

    def test_skips_apt_when_sudo_would_prompt_but_nothing_is_missing(self) -> None:
        self.stub_environment(installed=["binutils"], sudo_is_passwordless=False)

        result = self.run_script("binutils")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(self.logged_commands(), [])

    def test_fails_closed_without_passwordless_sudo(self) -> None:
        self.stub_environment(installed=["binutils"], sudo_is_passwordless=False)

        result = self.run_script("binutils", "libcap-dev")

        self.assertNotEqual(0, result.returncode)
        self.assertIn("without passwordless sudo: libcap-dev", result.stderr)
        self.assertEqual(self.logged_commands(), [])

    def test_installs_only_the_missing_packages(self) -> None:
        self.stub_environment(installed=["binutils"], sudo_is_passwordless=True)

        result = self.run_script("binutils", "libcap-dev", "pkg-config")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(
            self.logged_commands(),
            [
                "sudo env DEBIAN_FRONTEND=noninteractive apt-get update",
                "apt-get update",
                "sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y "
                "libcap-dev pkg-config",
                "apt-get install -y libcap-dev pkg-config",
            ],
        )

    def test_runs_apt_directly_as_root(self) -> None:
        self.stub_environment(installed=[], sudo_is_passwordless=False, is_root=True)

        result = self.run_script("libcap-dev")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual(
            self.logged_commands(),
            ["apt-get update", "apt-get install -y libcap-dev"],
        )

    def test_forwards_apt_argument_overrides(self) -> None:
        self.stub_environment(installed=[], sudo_is_passwordless=True)
        env = dict(os.environ)
        env["PATH"] = f"{self.bin_dir}{os.pathsep}{env['PATH']}"
        env["APT_UPDATE_ARGS"] = "-o Acquire::Retries=3"
        env["APT_INSTALL_ARGS"] = "--no-install-recommends"

        result = subprocess.run(
            ["bash", str(SCRIPT), "libcap-dev"],
            env=env,
            capture_output=True,
            text=True,
        )

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("apt-get update -o Acquire::Retries=3", self.logged_commands())
        self.assertIn(
            "apt-get install -y --no-install-recommends libcap-dev",
            self.logged_commands(),
        )

    def test_requires_at_least_one_package(self) -> None:
        self.stub_environment(installed=[], sudo_is_passwordless=True)

        result = self.run_script()

        self.assertNotEqual(0, result.returncode)
        self.assertIn("usage:", result.stderr)


class NoUnconditionalSudoAptTest(unittest.TestCase):
    """The callers must not reintroduce an unconditional `sudo apt-get`."""

    def test_musl_setup_uses_the_helper(self) -> None:
        contents = MUSL_SCRIPT.read_text()

        self.assertIn("install-apt-packages.sh", contents)
        self.assertNotIn("sudo apt-get", contents)

    def test_rust_release_uses_the_helper(self) -> None:
        contents = RUST_RELEASE.read_text()

        self.assertIn("install-apt-packages.sh", contents)
        self.assertNotIn("sudo apt-get", contents)


if __name__ == "__main__":
    unittest.main()
