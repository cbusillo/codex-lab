#!/usr/bin/env python3

import subprocess
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

import stage_npm_packages

WORKFLOW_ID = "test-workflow-run"
RELEASE_SOURCE_ID = "github-release:openai/codex:rust-v0.133.0-alpha.4"


def release_asset(
    name: str, size_in_bytes: int = 123
) -> stage_npm_packages.ReleaseAsset:
    return stage_npm_packages.ReleaseAsset(
        name=name,
        size_in_bytes=size_in_bytes,
        asset_id=f"id-{name}",
        digest=f"sha256:{name}",
        updated_at="2026-05-21T02:26:46Z",
    )


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

    def test_release_cache_is_complete_with_matching_marker_and_assets(self) -> None:
        artifact = stage_npm_packages.ReleaseArtifact(
            name="x86_64-unknown-linux-musl",
            size_in_bytes=123,
            assets=(release_asset("codex-package-x86_64-unknown-linux-musl.tar.gz"),),
        )
        with tempfile.TemporaryDirectory() as temp_dir:
            artifact_dir = Path(temp_dir) / artifact.name
            artifact_dir.mkdir()
            (artifact_dir / artifact.asset_names[0]).touch()
            stage_npm_packages.write_release_artifact_cache_marker(
                artifact_dir,
                RELEASE_SOURCE_ID,
                artifact,
            )

            self.assertTrue(
                stage_npm_packages.release_artifact_cache_is_complete(
                    artifact_dir,
                    RELEASE_SOURCE_ID,
                    artifact,
                )
            )

    def test_release_cache_is_incomplete_without_all_assets(self) -> None:
        artifact = stage_npm_packages.ReleaseArtifact(
            name="x86_64-unknown-linux-musl",
            size_in_bytes=123,
            assets=(
                release_asset("codex-package-x86_64-unknown-linux-musl.tar.gz"),
                release_asset(
                    "codex-responses-api-proxy-x86_64-unknown-linux-musl.zst"
                ),
            ),
        )
        with tempfile.TemporaryDirectory() as temp_dir:
            artifact_dir = Path(temp_dir) / artifact.name
            artifact_dir.mkdir()
            (artifact_dir / artifact.asset_names[0]).touch()
            stage_npm_packages.write_release_artifact_cache_marker(
                artifact_dir,
                RELEASE_SOURCE_ID,
                artifact,
            )

            self.assertFalse(
                stage_npm_packages.release_artifact_cache_is_complete(
                    artifact_dir,
                    RELEASE_SOURCE_ID,
                    artifact,
                )
            )

    def test_release_cache_is_incomplete_when_asset_identity_changes(self) -> None:
        artifact = stage_npm_packages.ReleaseArtifact(
            name="x86_64-unknown-linux-musl",
            size_in_bytes=123,
            assets=(release_asset("codex-package-x86_64-unknown-linux-musl.tar.gz"),),
        )
        replaced_artifact = stage_npm_packages.ReleaseArtifact(
            name=artifact.name,
            size_in_bytes=artifact.size_in_bytes,
            assets=(
                stage_npm_packages.ReleaseAsset(
                    name=artifact.asset_names[0],
                    size_in_bytes=123,
                    asset_id="new-asset-id",
                    digest="sha256:new-digest",
                    updated_at="2026-05-22T02:26:46Z",
                ),
            ),
        )
        with tempfile.TemporaryDirectory() as temp_dir:
            artifact_dir = Path(temp_dir) / artifact.name
            artifact_dir.mkdir()
            (artifact_dir / artifact.asset_names[0]).touch()
            stage_npm_packages.write_release_artifact_cache_marker(
                artifact_dir,
                RELEASE_SOURCE_ID,
                artifact,
            )

            self.assertFalse(
                stage_npm_packages.release_artifact_cache_is_complete(
                    artifact_dir,
                    RELEASE_SOURCE_ID,
                    replaced_artifact,
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

    def test_select_release_artifacts_requires_codex_package_assets(self) -> None:
        assets = [
            release_asset(f"codex-package-{target}.tar.gz", index + 1)
            for index, target in enumerate(stage_npm_packages.BINARY_TARGETS)
        ]

        with mock.patch.object(
            stage_npm_packages,
            "list_release_assets",
            return_value=assets,
        ):
            selected = stage_npm_packages.select_release_artifacts(
                "rust-v0.133.0-alpha.4",
                [stage_npm_packages.CODEX_PACKAGE_COMPONENT],
            )

        self.assertEqual(
            selected,
            [
                stage_npm_packages.ReleaseArtifact(
                    name=target,
                    size_in_bytes=index + 1,
                    assets=(
                        release_asset(f"codex-package-{target}.tar.gz", index + 1),
                    ),
                )
                for index, target in enumerate(stage_npm_packages.BINARY_TARGETS)
            ],
        )

    def test_select_release_artifacts_reports_missing_asset(self) -> None:
        with mock.patch.object(
            stage_npm_packages,
            "list_release_assets",
            return_value=[],
        ):
            with self.assertRaisesRegex(
                FileNotFoundError,
                "codex-package-x86_64-unknown-linux-musl.tar.gz",
            ):
                stage_npm_packages.select_release_artifacts(
                    "rust-v0.133.0-alpha.4",
                    [stage_npm_packages.CODEX_PACKAGE_COMPONENT],
                )

    def test_download_release_artifacts_downloads_assets_by_target(self) -> None:
        artifact = stage_npm_packages.ReleaseArtifact(
            name="x86_64-unknown-linux-musl",
            size_in_bytes=123,
            assets=(release_asset("codex-package-x86_64-unknown-linux-musl.tar.gz"),),
        )
        commands: list[list[str]] = []

        def fake_check_call(cmd: list[str]) -> None:
            commands.append(cmd)
            artifact_dir = Path(cmd[cmd.index("--dir") + 1])
            artifact_dir.mkdir(parents=True, exist_ok=True)
            (artifact_dir / artifact.asset_names[0]).touch()

        with tempfile.TemporaryDirectory() as temp_dir:
            with mock.patch.object(
                stage_npm_packages.subprocess,
                "check_call",
                side_effect=fake_check_call,
            ):
                stage_npm_packages.download_release_artifacts(
                    "rust-v0.133.0-alpha.4",
                    Path(temp_dir),
                    [artifact],
                )

            artifact_dir = Path(temp_dir) / artifact.name
            self.assertEqual(
                commands,
                [
                    [
                        "gh",
                        "release",
                        "download",
                        "rust-v0.133.0-alpha.4",
                        "--repo",
                        stage_npm_packages.GITHUB_REPO,
                        "--pattern",
                        artifact.asset_names[0],
                        "--dir",
                        str(artifact_dir),
                        "--clobber",
                    ]
                ],
            )
            self.assertTrue(
                stage_npm_packages.release_artifact_cache_is_complete(
                    artifact_dir,
                    RELEASE_SOURCE_ID,
                    artifact,
                )
            )


if __name__ == "__main__":
    unittest.main()
