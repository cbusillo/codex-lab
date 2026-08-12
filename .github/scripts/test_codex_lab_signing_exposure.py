#!/usr/bin/env python3
"""Bound the Codex Lab signing-key exposure documented as gate #614.

The shared login signing keychain cannot be isolated from the workflows: it
needs an operator to move the Developer ID key into a dedicated keychain and to
give release signing its own runner. These tests do the part that *is* code:
they prevent embedded keychain-password assumptions, stop the blast radius from
growing silently, and keep the documented gate visible while the exposure is
still real.

See the "Signing Key Exposure" section of .github/workflows/README.md.
"""

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"
README = WORKFLOWS / "README.md"
SIGNING_RUNNER_LABEL = "codex-lab-app"
# Anything that drives a signing identity.
SIGNING_MARKERS = (
    re.compile(r"security\s+find-identity"),
    re.compile(r"Developer ID Application"),
    re.compile(r"macos-signing/sign_macos_code\.sh"),
)
KEYCHAIN_PASSWORD_MARKERS = (
    re.compile(r"security\s+unlock-keychain"),
    re.compile(r"security\s+create-keychain"),
)
TRIGGER_BLOCK = re.compile(r"^on:\n(?P<triggers>(?:[ \t].*\n|\n)*)", re.MULTILINE)


def pull_request_triggered(contents: str) -> bool:
    match = TRIGGER_BLOCK.search(contents)
    if match is None:
        return False
    return any(
        line.strip().startswith("pull_request")
        for line in match.group("triggers").splitlines()
        if len(line) - len(line.lstrip()) == 2
    )


def workflow_paths() -> list[Path]:
    return sorted({*WORKFLOWS.glob("*.yml"), *WORKFLOWS.glob("*.yaml")})


def signs_code(contents: str) -> bool:
    return any(marker.search(contents) for marker in SIGNING_MARKERS)


class SigningExposureBoundaryTest(unittest.TestCase):
    def test_no_workflow_embeds_keychain_password_operations(self) -> None:
        for path in workflow_paths():
            contents = path.read_text()
            with self.subTest(workflow=path.name):
                self.assertFalse(
                    any(marker.search(contents) for marker in KEYCHAIN_PASSWORD_MARKERS),
                    f"{path.name} embeds a keychain password operation",
                )

    def test_no_pull_request_triggered_workflow_signs_code(self) -> None:
        for path in workflow_paths():
            contents = path.read_text()
            if not pull_request_triggered(contents):
                continue
            with self.subTest(workflow=path.name):
                self.assertFalse(
                    signs_code(contents),
                    f"{path.name} is pull-request triggered and signs code; "
                    "gate #614 assumes signing stays off the PR path",
                )

    def test_codex_lab_release_is_not_pull_request_triggered(self) -> None:
        contents = (WORKFLOWS / "codex-lab-release.yml").read_text()

        self.assertTrue(signs_code(contents))
        self.assertFalse(pull_request_triggered(contents))

    def test_codex_lab_app_still_builds_without_signing(self) -> None:
        contents = (WORKFLOWS / "codex-lab-app.yml").read_text()

        self.assertIn(SIGNING_RUNNER_LABEL, contents)
        self.assertFalse(signs_code(contents))

    def test_signing_is_confined_to_a_single_codex_lab_workflow(self) -> None:
        signing_workflows = {
            path.name
            for path in workflow_paths()
            if SIGNING_RUNNER_LABEL in path.read_text() and signs_code(path.read_text())
        }

        self.assertEqual(signing_workflows, {"codex-lab-release.yml"})


class SigningExposureGateDocumentedTest(unittest.TestCase):
    """The gate stays documented for as long as the exposure exists."""

    def setUp(self) -> None:
        self.release = (WORKFLOWS / "codex-lab-release.yml").read_text()
        # The gate is prose, so match it with the hard wrapping collapsed.
        self.readme = " ".join(README.read_text().split())

    def test_release_requires_an_already_unlocked_keychain(self) -> None:
        self.assertNotIn("security unlock-keychain", self.release)
        self.assertIn(
            'security show-keychain-info "$signing_keychain"', self.release
        )
        self.assertIn("Signing keychain is unavailable or locked", self.release)
        self.assertIn(
            'security set-keychain-settings -lut 21600 "$signing_keychain"',
            self.release,
        )

    def test_the_gate_is_documented(self) -> None:
        self.assertIn("Signing Key Exposure", self.readme)
        self.assertIn("codex-lab/issues/614", self.readme)

    def test_the_gate_names_both_required_operator_actions(self) -> None:
        self.assertIn("dedicated signing keychain", self.readme)
        self.assertIn("its own runner label", self.readme)


if __name__ == "__main__":
    unittest.main()
