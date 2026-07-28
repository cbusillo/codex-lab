#!/usr/bin/env python3

"""Keep the Windows installer's state home aligned with install.sh and the runtime.

`install.ps1` used to resolve `CODEX_HOME` and default to `~/.codex`, while
`install.sh` and `codex-rs/utils/home-dir` both resolve `CODEX_LAB_HOME` and
default to `~/.codex-lab`. A Windows install therefore wrote its standalone
package into a directory Codex Lab never reads, so updates and rollbacks could
not see the installed releases.
"""

import os
from pathlib import Path
import re
import shutil
import subprocess
import unittest


INSTALL_PS1 = Path(__file__).with_name("install.ps1")
INSTALL_SH = Path(__file__).with_name("install.sh")

# Anchored on the statement that follows the block, because the `} else {` line
# would otherwise end the match halfway through the assignment.
HOME_ASSIGNMENT = re.compile(
    r"^\$codexHome = if \(.*?^\}$(?=\n\$standaloneRoot)", re.DOTALL | re.MULTILINE
)

# `Join-Path` validates drive letters, so a Windows-shaped profile path fails
# under the `pwsh` builds that run on the Linux and macOS CI images. The block
# under test only chooses a directory *name*, so a native profile path exercises
# it identically on every platform.
USER_PROFILE = "/tmp/codex-lab-install-test-profile"


def home_assignment() -> str:
    """The shipped `$codexHome` block, so the behavior test cannot drift from the file."""

    match = HOME_ASSIGNMENT.search(INSTALL_PS1.read_text(encoding="utf-8"))
    if match is None:
        raise AssertionError("install.ps1 no longer contains a $codexHome assignment")
    return match.group(0)


def resolve_home(**env_overrides: str | None) -> str:
    """Evaluate the shipped assignment under `pwsh` and return the resolved path."""

    env = {key: value for key, value in os.environ.items()}
    env["USERPROFILE"] = USER_PROFILE
    for key, value in env_overrides.items():
        if value is None:
            env.pop(key, None)
        else:
            env[key] = value
    script = f"{home_assignment()}\nWrite-Output $codexHome\n"
    result = subprocess.run(
        ["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    return result.stdout.strip()


class InstallPs1TextTest(unittest.TestCase):
    def setUp(self) -> None:
        self.script = INSTALL_PS1.read_text(encoding="utf-8")

    def test_state_home_env_var_matches_install_sh(self) -> None:
        self.assertIn("$env:CODEX_LAB_HOME", self.script)
        self.assertIn("CODEX_LAB_HOME", INSTALL_SH.read_text(encoding="utf-8"))

    def test_no_legacy_codex_home_env_var(self) -> None:
        # `CODEX_HOME` is not read by the runtime, so honoring it here would
        # install into a directory Codex Lab never opens.
        self.assertNotRegex(self.script, r"\$env:CODEX_HOME\b")

    def test_default_state_home_is_codex_lab(self) -> None:
        self.assertIn('Join-Path $env:USERPROFILE ".codex-lab"', self.script)
        self.assertNotIn('Join-Path $env:USERPROFILE ".codex"', self.script)


@unittest.skipIf(shutil.which("pwsh") is None, "pwsh is not installed")
class InstallPs1BehaviorTest(unittest.TestCase):
    def setUp(self) -> None:
        self.default_home = str(Path(USER_PROFILE) / ".codex-lab")

    def test_unset_state_home_defaults_under_user_profile(self) -> None:
        self.assertEqual(
            resolve_home(CODEX_LAB_HOME=None, CODEX_HOME=None),
            self.default_home,
        )

    def test_state_home_env_var_overrides_the_default(self) -> None:
        self.assertEqual(
            resolve_home(CODEX_LAB_HOME="/srv/lab", CODEX_HOME=None),
            "/srv/lab",
        )

    def test_blank_state_home_env_var_falls_back_to_the_default(self) -> None:
        self.assertEqual(
            resolve_home(CODEX_LAB_HOME="   ", CODEX_HOME=None),
            self.default_home,
        )

    def test_legacy_codex_home_is_ignored(self) -> None:
        self.assertEqual(
            resolve_home(CODEX_LAB_HOME=None, CODEX_HOME="/srv/legacy"),
            self.default_home,
        )


if __name__ == "__main__":
    unittest.main()
