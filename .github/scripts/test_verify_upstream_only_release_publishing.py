#!/usr/bin/env python3
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import publish_r2_release
import verify_upstream_only_release_publishing as publishing


GUARDED_R2_RELEASE = (
    "on:\n"
    "  workflow_call:\n"
    "jobs:\n"
    "  publish:\n"
    "    runs-on: ubuntu-latest\n"
    "    if: ${{ github.repository == 'openai/codex' }}\n"
    "    steps:\n"
    "      - run: echo publish\n"
)
UNGUARDED_R2_RELEASE = (
    "on:\n"
    "  workflow_call:\n"
    "jobs:\n"
    "  publish:\n"
    "    runs-on: ubuntu-latest\n"
    "    steps:\n"
    "      - run: echo publish\n"
)


class VerifyUpstreamOnlyReleasePublishingTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.workflows_dir = Path(self.temp_dir.name) / ".github/workflows"
        self.workflows_dir.mkdir(parents=True)

    def write_workflow(self, name: str, contents: str) -> None:
        (self.workflows_dir / name).write_text(contents)

    def test_accepts_guarded_caller_and_publisher(self) -> None:
        self.write_workflow("r2-release.yml", GUARDED_R2_RELEASE)
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  release:\n"
            "    runs-on: ubuntu-latest\n"
            "  publish-r2:\n"
            "    needs: [release]\n"
            "    if: ${{ github.repository == 'openai/codex' }}\n"
            "    uses: ./.github/workflows/r2-release.yml\n"
            "    secrets: inherit\n",
        )

        self.assertEqual(publishing.find_violations(self.workflows_dir), [])

    def test_accepts_multiline_guard_condition(self) -> None:
        self.write_workflow("r2-release.yml", GUARDED_R2_RELEASE)
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  publish-r2:\n"
            "    if: >-\n"
            "      ${{\n"
            "        github.repository == 'openai/codex' &&\n"
            "        needs.release.result == 'success'\n"
            "      }}\n"
            "    uses: ./.github/workflows/r2-release.yml\n",
        )

        self.assertEqual(publishing.find_violations(self.workflows_dir), [])

    def test_rejects_unguarded_caller(self) -> None:
        self.write_workflow("r2-release.yml", GUARDED_R2_RELEASE)
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  publish-r2:\n"
            "    needs: [release]\n"
            "    uses: ./.github/workflows/r2-release.yml\n",
        )

        violations = publishing.find_violations(self.workflows_dir)
        self.assertEqual(len(violations), 1)
        self.assertIn("calls r2-release.yml", violations[0].reason)

    def test_rejects_guard_for_a_different_repository(self) -> None:
        self.write_workflow("r2-release.yml", GUARDED_R2_RELEASE)
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  publish-r2:\n"
            "    if: ${{ github.repository == 'cbusillo/codex-lab' }}\n"
            "    uses: ./.github/workflows/r2-release.yml\n",
        )

        violations = publishing.find_violations(self.workflows_dir)
        self.assertEqual(len(violations), 1)
        self.assertIn("calls r2-release.yml", violations[0].reason)

    def test_rejects_unguarded_publisher_job(self) -> None:
        self.write_workflow("r2-release.yml", UNGUARDED_R2_RELEASE)

        violations = publishing.find_violations(self.workflows_dir)
        self.assertEqual(len(violations), 1)
        self.assertIn("runs upstream-only publishing", violations[0].reason)

    def test_reports_missing_upstream_only_workflow(self) -> None:
        violations = publishing.find_violations(self.workflows_dir)
        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].reason, "upstream-only workflow is missing")

    def test_ignores_step_level_repository_conditions(self) -> None:
        self.write_workflow("r2-release.yml", GUARDED_R2_RELEASE)
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  publish-r2:\n"
            "    uses: ./.github/workflows/r2-release.yml\n"
            "  unrelated:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - if: ${{ github.repository == 'openai/codex' }}\n"
            "        run: echo hello\n",
        )

        violations = publishing.find_violations(self.workflows_dir)
        self.assertEqual(len(violations), 1)
        self.assertIn("publish-r2", violations[0].reason)


class RepositoryWorkflowGuardTest(unittest.TestCase):
    """The checked-in workflows must satisfy the verifier."""

    def test_repository_workflows_are_guarded(self) -> None:
        self.assertEqual(
            publishing.find_violations(publishing.ROOT / ".github/workflows"),
            [],
        )


class PublishR2ReleaseGuardTest(unittest.TestCase):
    def test_allows_upstream_repository(self) -> None:
        publish_r2_release.require_upstream_repository("openai/codex")

    def test_rejects_fork_repository(self) -> None:
        with self.assertRaises(publish_r2_release.PublishError) as error:
            publish_r2_release.require_upstream_repository("cbusillo/codex-lab")
        self.assertIn("cbusillo/codex-lab", str(error.exception))

    def test_rejects_unset_repository(self) -> None:
        with self.assertRaises(publish_r2_release.PublishError) as error:
            publish_r2_release.require_upstream_repository(None)
        self.assertIn("unset GITHUB_REPOSITORY", str(error.exception))

    def test_main_fails_before_touching_credentials(self) -> None:
        argv = [
            "publish_r2_release.py",
            "--tag",
            "rust-v1.2.3",
            "--make-latest",
            "true",
            "--prerelease",
            "false",
        ]
        environment = {
            "GITHUB_REPOSITORY": "cbusillo/codex-lab",
            "GH_TOKEN": "token",
            "AWS_ENDPOINT_URL": "https://example.invalid",
        }
        with (
            patch.object(publish_r2_release.sys, "argv", argv),
            patch.dict(os.environ, environment, clear=False),
            patch.object(publish_r2_release, "download_assets") as download_assets,
        ):
            self.assertEqual(publish_r2_release.main(), 1)
        download_assets.assert_not_called()


if __name__ == "__main__":
    unittest.main()
