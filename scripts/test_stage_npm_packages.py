#!/usr/bin/env python3

import subprocess
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

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


class StagePackageTests(unittest.TestCase):
    def test_stage_single_package_uses_distinct_paths_and_cleans_up(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            output_dir = root / "out"
            output_dir.mkdir()
            vendor_src = root / "vendor"
            vendor_src.mkdir()
            commands: list[list[str]] = []

            def fake_run_command(cmd: list[str]) -> str:
                commands.append(cmd)
                return "packed\n"

            with mock.patch.object(
                stage_npm_packages,
                "run_command",
                side_effect=fake_run_command,
            ):
                staged = stage_npm_packages.stage_single_package(
                    "codex-linux-x64",
                    "0.1.0",
                    output_dir,
                    root,
                    vendor_src,
                    keep_staging_dirs=False,
                )

            self.assertEqual(staged.package, "codex-linux-x64")
            self.assertEqual(
                staged.pack_output, output_dir / "codex-npm-linux-x64-0.1.0.tgz"
            )
            self.assertIn("packed", staged.output)
            self.assertEqual(len(commands), 1)
            command = commands[0]
            self.assertIn("--vendor-src", command)
            self.assertIn(str(vendor_src), command)
            staging_dir = Path(command[command.index("--staging-dir") + 1])
            self.assertFalse(staging_dir.exists())

    def test_run_command_captures_command_and_output(self) -> None:
        output = stage_npm_packages.run_command(
            ["python3", "-c", "print('packed')"],
        )

        self.assertIn("+ python3 -c", output)
        self.assertIn("packed", output)

    def test_stage_single_package_cleans_up_after_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            output_dir = root / "out"
            output_dir.mkdir()
            staging_dirs: list[Path] = []

            def fake_run_command(cmd: list[str]) -> str:
                staging_dirs.append(Path(cmd[cmd.index("--staging-dir") + 1]))
                raise RuntimeError("pack failed")

            with mock.patch.object(
                stage_npm_packages,
                "run_command",
                side_effect=fake_run_command,
            ):
                with self.assertRaisesRegex(RuntimeError, "pack failed"):
                    stage_npm_packages.stage_single_package(
                        "codex",
                        "0.1.0",
                        output_dir,
                        root,
                        vendor_src=None,
                        keep_staging_dirs=False,
                    )

            self.assertEqual(len(staging_dirs), 1)
            self.assertFalse(staging_dirs[0].exists())

    def test_stage_single_package_includes_subprocess_output_on_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            output_dir = root / "out"
            output_dir.mkdir()

            def fake_run_command(cmd: list[str]) -> str:
                raise subprocess.CalledProcessError(
                    1,
                    cmd,
                    output="+ fake command\nnpm failed\n",
                )

            with mock.patch.object(
                stage_npm_packages,
                "run_command",
                side_effect=fake_run_command,
            ):
                with self.assertRaises(stage_npm_packages.PackageStageError) as context:
                    stage_npm_packages.stage_single_package(
                        "codex",
                        "0.1.0",
                        output_dir,
                        root,
                        vendor_src=None,
                        keep_staging_dirs=False,
                    )

            self.assertEqual(context.exception.package, "codex")
            self.assertIn("npm failed", context.exception.output)

    def test_package_staging_can_run_concurrently(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            output_dir = root / "out"
            output_dir.mkdir()
            barrier = threading.Barrier(2)
            active = 0
            max_active = 0
            lock = threading.Lock()

            def fake_run_command(cmd: list[str]) -> str:
                nonlocal active, max_active
                with lock:
                    active += 1
                    max_active = max(max_active, active)
                barrier.wait(timeout=5)
                with lock:
                    active -= 1
                return "packed\n"

            with mock.patch.object(
                stage_npm_packages,
                "run_command",
                side_effect=fake_run_command,
            ):
                with stage_npm_packages.ThreadPoolExecutor(max_workers=2) as executor:
                    futures = [
                        executor.submit(
                            stage_npm_packages.stage_single_package,
                            package,
                            "0.1.0",
                            output_dir,
                            root,
                            None,
                            False,
                        )
                        for package in ["codex", "codex-linux-x64"]
                    ]
                    staged = [future.result() for future in futures]

            self.assertEqual(
                [package.package for package in staged], ["codex", "codex-linux-x64"]
            )
            self.assertEqual(max_active, 2)

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
