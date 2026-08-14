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


class UpstreamOwnedMutationGuardTest(unittest.TestCase):
    """Jobs that mutate OpenAI-owned state are found by fingerprint, not name."""

    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.workflows_dir = Path(self.temp_dir.name) / ".github/workflows"
        self.workflows_dir.mkdir(parents=True)
        (self.workflows_dir / "r2-release.yml").write_text(GUARDED_R2_RELEASE)

    def write_workflow(self, name: str, contents: str) -> None:
        (self.workflows_dir / name).write_text(contents)

    def assert_single_violation(self, mutation: str) -> None:
        violations = publishing.find_violations(self.workflows_dir)
        self.assertEqual(len(violations), 1, violations)
        self.assertIn(f"publishes to {mutation}", violations[0].reason)

    def test_rejects_unguarded_openai_npm_scope(self) -> None:
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  publish-npm:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - uses: actions/setup-node@v6\n"
            "        with:\n"
            '          scope: "@openai"\n'
            "      - run: npm publish dist/npm/codex.tgz\n",
        )

        self.assert_single_violation("the @openai npm scope")

    def test_rejects_unguarded_winget_publish(self) -> None:
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  winget:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - uses: vedantmgoyal9/winget-releaser@abc\n"
            "        with:\n"
            "          identifier: OpenAI.Codex\n",
        )

        self.assert_single_violation("the OpenAI.Codex WinGet manifest")

    def test_rejects_unguarded_dev_website_deploy_hook(self) -> None:
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  deploy-dev-website:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - env:\n"
            "          DEV_WEBSITE_VERCEL_DEPLOY_HOOK_URL: ${{ secrets.DEV_WEBSITE_VERCEL_DEPLOY_HOOK_URL }}\n"
            '        run: curl -X POST "$DEV_WEBSITE_VERCEL_DEPLOY_HOOK_URL"\n',
        )

        self.assert_single_violation("the developers.openai.com deploy hook")

    def test_rejects_unguarded_r2_bucket_credentials(self) -> None:
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  mirror:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - env:\n"
            "          AWS_ACCESS_KEY_ID: ${{ secrets.R2_RELEASES_ACCESS_KEY_ID }}\n"
            "        run: aws s3 cp dist s3://releases --recursive\n",
        )

        self.assert_single_violation("the upstream R2 release bucket credential")

    def test_a_renamed_job_still_needs_the_guard(self) -> None:
        self.write_workflow(
            "some-other-workflow.yml",
            "jobs:\n"
            "  totally-innocuous-name:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - uses: vedantmgoyal9/winget-releaser@abc\n"
            "        with:\n"
            "          identifier: OpenAI.Codex\n"
            "          fork-user: openai-oss-forks\n"
            "          token: ${{ secrets.WINGET_PUBLISH_PAT }}\n",
        )

        violations = publishing.find_violations(self.workflows_dir)
        self.assertEqual(len(violations), 3, violations)
        for violation in violations:
            self.assertIn("totally-innocuous-name", violation.reason)

    def test_accepts_guarded_upstream_owned_mutations(self) -> None:
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  winget:\n"
            "    runs-on: ubuntu-latest\n"
            "    if: >-\n"
            "      ${{\n"
            "        github.repository == 'openai/codex' &&\n"
            "        !cancelled()\n"
            "      }}\n"
            "    steps:\n"
            "      - uses: vedantmgoyal9/winget-releaser@abc\n"
            "        with:\n"
            "          identifier: OpenAI.Codex\n"
            "          fork-user: openai-oss-forks\n"
            "          token: ${{ secrets.WINGET_PUBLISH_PAT }}\n",
        )

        self.assertEqual(publishing.find_violations(self.workflows_dir), [])

    def test_ignores_jobs_without_an_upstream_owned_mutation(self) -> None:
        self.write_workflow(
            "rust-release.yml",
            "jobs:\n"
            "  publish-dotslash:\n"
            "    runs-on: ubuntu-latest\n"
            "    steps:\n"
            "      - uses: facebook/dotslash-publish-release@abc\n"
            "        with:\n"
            "          tag: ${{ github.ref_name }}\n",
        )

        self.assertEqual(publishing.find_violations(self.workflows_dir), [])


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
            "--stage",
            "assets",
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
