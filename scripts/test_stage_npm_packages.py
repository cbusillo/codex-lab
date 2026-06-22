#!/usr/bin/env python3

import tempfile
import unittest
from pathlib import Path

import stage_npm_packages

WORKFLOW_ID = "26201494185"


class ArtifactCacheMarkerTests(unittest.TestCase):
    def test_cache_is_complete_with_matching_marker_and_payload(self) -> None:
        artifact = stage_npm_packages.WorkflowArtifact(
            name="x86_64-unknown-linux-musl",
            size_in_bytes=123,
        )
        with tempfile.TemporaryDirectory() as temp_dir:
            artifact_dir = Path(temp_dir) / artifact.name
            artifact_dir.mkdir()
            (artifact_dir / "codex-package-x86_64-unknown-linux-musl.tar.gz").touch()
            stage_npm_packages.write_artifact_cache_marker(
                artifact_dir,
                WORKFLOW_ID,
                artifact,
            )

            self.assertTrue(
                stage_npm_packages.artifact_cache_is_complete(
                    artifact_dir,
                    WORKFLOW_ID,
                    artifact,
                )
            )

    def test_cache_is_incomplete_without_payload(self) -> None:
        artifact = stage_npm_packages.WorkflowArtifact(
            name="x86_64-unknown-linux-musl",
            size_in_bytes=123,
        )
        with tempfile.TemporaryDirectory() as temp_dir:
            artifact_dir = Path(temp_dir) / artifact.name
            artifact_dir.mkdir()
            stage_npm_packages.write_artifact_cache_marker(
                artifact_dir,
                WORKFLOW_ID,
                artifact,
            )

            self.assertFalse(
                stage_npm_packages.artifact_cache_is_complete(
                    artifact_dir,
                    WORKFLOW_ID,
                    artifact,
                )
            )

    def test_cache_is_incomplete_when_workflow_id_does_not_match(self) -> None:
        artifact = stage_npm_packages.WorkflowArtifact(
            name="x86_64-unknown-linux-musl",
            size_in_bytes=123,
        )
        with tempfile.TemporaryDirectory() as temp_dir:
            artifact_dir = Path(temp_dir) / artifact.name
            artifact_dir.mkdir()
            (artifact_dir / "codex-package-x86_64-unknown-linux-musl.tar.gz").touch()
            stage_npm_packages.write_artifact_cache_marker(
                artifact_dir,
                WORKFLOW_ID,
                artifact,
            )

            self.assertFalse(
                stage_npm_packages.artifact_cache_is_complete(
                    artifact_dir,
                    "another-workflow",
                    artifact,
                )
            )

    def test_cache_is_incomplete_when_marker_does_not_match(self) -> None:
        artifact = stage_npm_packages.WorkflowArtifact(
            name="x86_64-unknown-linux-musl",
            size_in_bytes=123,
        )
        stale_artifact = stage_npm_packages.WorkflowArtifact(
            name=artifact.name,
            size_in_bytes=456,
        )
        with tempfile.TemporaryDirectory() as temp_dir:
            artifact_dir = Path(temp_dir) / artifact.name
            artifact_dir.mkdir()
            (artifact_dir / "codex-package-x86_64-unknown-linux-musl.tar.gz").touch()
            stage_npm_packages.write_artifact_cache_marker(
                artifact_dir,
                WORKFLOW_ID,
                stale_artifact,
            )

            self.assertFalse(
                stage_npm_packages.artifact_cache_is_complete(
                    artifact_dir,
                    WORKFLOW_ID,
                    artifact,
                )
            )


if __name__ == "__main__":
    unittest.main()
