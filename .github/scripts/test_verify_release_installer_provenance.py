#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import verify_release_installer_provenance as provenance


GUARDED_STAGING = (
    "jobs:\n"
    "  release:\n"
    "    runs-on: ubuntu-latest\n"
    "    steps:\n"
    "      - name: Stage installer scripts\n"
    "        if: ${{ github.repository == 'openai/codex' }}\n"
    "        run: |\n"
    "          cp scripts/install/install.sh dist/install.sh\n"
    "          cp scripts/install/install.ps1 dist/install.ps1\n"
)
UNGUARDED_STAGING = (
    "jobs:\n"
    "  release:\n"
    "    runs-on: ubuntu-latest\n"
    "    steps:\n"
    "      - name: Stage installer scripts\n"
    "        run: |\n"
    "          cp scripts/install/install.sh dist/install.sh\n"
    "          cp scripts/install/install.ps1 dist/install.ps1\n"
)
FORK_GUARDED_STAGING = (
    "jobs:\n"
    "  release:\n"
    "    runs-on: ubuntu-latest\n"
    "    steps:\n"
    "      - name: Stage installer scripts\n"
    "        if: ${{ github.repository == 'cbusillo/codex-lab' }}\n"
    "        run: cp scripts/install/install.sh dist/install.sh\n"
)
CODEX_LAB_RELEASE = (
    "jobs:\n"
    "  publish-prerelease:\n"
    "    runs-on: ubuntu-latest\n"
    "    steps:\n"
    "      - name: Publish prerelease\n"
    "        run: |\n"
    '          gh release create "$release_tag" \\\n'
    '            --repo "$GITHUB_REPOSITORY" \\\n'
    "            dist/codex-lab-app-aarch64-apple-darwin.zip \\\n"
    "            dist/codex-lab-distribution.json \\\n"
    "            dist/SHA256SUMS\n"
)


class InstallerStagingGuardTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.workflows = Path(self.temp_dir.name) / ".github/workflows"
        self.workflows.mkdir(parents=True)
        (self.workflows / "codex-lab-release.yml").write_text(CODEX_LAB_RELEASE)
        self.patcher = patch.object(provenance, "WORKFLOWS", self.workflows)
        self.patcher.start()
        self.addCleanup(self.patcher.stop)

    def write_workflow(self, name: str, contents: str) -> None:
        (self.workflows / name).write_text(contents)

    def test_accepts_upstream_guarded_staging(self) -> None:
        self.write_workflow("rust-release.yml", GUARDED_STAGING)

        self.assertEqual(provenance.find_violations(), [])

    def test_rejects_unguarded_staging(self) -> None:
        self.write_workflow("rust-release.yml", UNGUARDED_STAGING)

        violations = provenance.find_violations()
        self.assertEqual(len(violations), 1)
        self.assertIn("without an openai/codex guard", violations[0].reason)

    def test_rejects_a_fork_guard_on_upstream_installers(self) -> None:
        self.write_workflow("rust-release.yml", FORK_GUARDED_STAGING)

        violations = provenance.find_violations()
        self.assertEqual(len(violations), 1)
        self.assertIn("without an openai/codex guard", violations[0].reason)

    def test_ignores_steps_that_only_run_the_installer(self) -> None:
        self.write_workflow(
            "install-smoke.yml",
            "jobs:\n"
            "  smoke:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - run: bash scripts/install/install.sh --version 1.2.3\n",
        )

        self.assertEqual(provenance.find_violations(), [])


class CodexLabReleaseAssetTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.workflows = Path(self.temp_dir.name) / ".github/workflows"
        self.workflows.mkdir(parents=True)
        self.patcher = patch.object(provenance, "WORKFLOWS", self.workflows)
        self.patcher.start()
        self.addCleanup(self.patcher.stop)

    def test_accepts_codex_lab_owned_assets(self) -> None:
        (self.workflows / "codex-lab-release.yml").write_text(CODEX_LAB_RELEASE)

        self.assertEqual(provenance.find_codex_lab_asset_violations(), [])

    def test_rejects_an_upstream_installer_asset(self) -> None:
        (self.workflows / "codex-lab-release.yml").write_text(
            CODEX_LAB_RELEASE.replace(
                "            dist/SHA256SUMS\n",
                "            dist/SHA256SUMS \\\n            dist/install.sh\n",
            )
        )

        violations = provenance.find_codex_lab_asset_violations()
        self.assertEqual(len(violations), 1)
        self.assertIn("non-Codex Lab release asset 'install.sh'", violations[0].reason)

    def test_reports_a_missing_publish_step(self) -> None:
        (self.workflows / "codex-lab-release.yml").write_text(
            "jobs:\n  noop:\n    runs-on: ubuntu-latest\n"
        )

        violations = provenance.find_codex_lab_asset_violations()
        self.assertEqual(len(violations), 1)
        self.assertIn("no `gh release create` step", violations[0].reason)


class RepositoryProvenanceTest(unittest.TestCase):
    """The checked-in workflows and installers must satisfy the verifier."""

    def test_repository_is_clean(self) -> None:
        self.assertEqual(provenance.find_violations(), [])

    def test_installers_still_resolve_only_upstream_downloads(self) -> None:
        for installer in provenance.INSTALLER_SOURCES:
            with self.subTest(installer=installer.name):
                self.assertEqual(
                    provenance.installer_downloads_from(installer),
                    {provenance.UPSTREAM_REPOSITORY},
                )


if __name__ == "__main__":
    unittest.main()
