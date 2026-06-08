#!/usr/bin/env python3

from pathlib import Path
from types import SimpleNamespace
from zipfile import BadZipFile
from zipfile import ZIP_DEFLATED
from zipfile import ZipFile
from zipfile import ZipInfo
import contextlib
import io
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.distribution_manifest import APP_ZIP
from codex_lab_package.distribution_manifest import MANIFEST_NAME
from codex_lab_package.distribution_manifest import SHIM_ZIP
from codex_lab_package.distribution_manifest import build_manifest
from codex_lab_package.distribution_manifest import sha256_file
from codex_lab_package.installer import DOWNLOAD_TIMEOUT_SECONDS
from codex_lab_package.installer import CodexLabReleaseSummary
from codex_lab_package.installer import CodexLabUpdateError
from codex_lab_package.installer import check_for_update
from codex_lab_package.installer import codex_lab_release_order
from codex_lab_package.installer import download_json_url
from codex_lab_package.installer import download_url
from codex_lab_package.installer import github_releases_url
from codex_lab_package.installer import install_from_manifest_url
from codex_lab_package.installer import latest_release_tag
from codex_lab_package.installer import manifest_url_for_latest_release
from codex_lab_package.installer import manifest_url_for_release_tag
from codex_lab_package.installer import read_install_state
from codex_lab_package.installer import replace_path
from codex_lab_package.installer import select_latest_lab_release_tag
from codex_lab_package.installer import update_from_latest_release
from codex_lab_package.layout import CodexLabAppOptions
from codex_lab_package.layout import build_codex_lab_app
import install_codex_lab as install_codex_lab_cli


class CodexLabInstallerTest(unittest.TestCase):
    def test_download_url_uses_timeout(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dest = Path(temp_dir) / "artifact.zip"
            response = FakeHttpResponse(b"artifact bytes")

            with mock.patch(
                "codex_lab_package.installer.urllib.request.urlopen",
                return_value=response,
            ) as urlopen:
                download_url("https://github.com/example/release/artifact.zip", dest)

            urlopen.assert_called_once()
            _, kwargs = urlopen.call_args
            self.assertEqual(kwargs["timeout"], DOWNLOAD_TIMEOUT_SECONDS)
            self.assertEqual(dest.read_bytes(), b"artifact bytes")

    def test_download_json_url_uses_github_headers_and_timeout(self) -> None:
        response = FakeHttpResponse(b'{"ok": true}')

        with mock.patch(
            "codex_lab_package.installer.urllib.request.urlopen",
            return_value=response,
        ) as urlopen:
            body = download_json_url(
                "https://api.github.com/repos/example/repo/releases"
            )

        urlopen.assert_called_once()
        request = urlopen.call_args.args[0]
        _, kwargs = urlopen.call_args
        self.assertEqual(kwargs["timeout"], DOWNLOAD_TIMEOUT_SECONDS)
        self.assertEqual(request.get_header("Accept"), "application/vnd.github+json")
        self.assertEqual(request.get_header("User-agent"), "codex-lab-installer/0")
        self.assertEqual(body, {"ok": True})

    def test_select_latest_lab_release_tag_prefers_newest_uploaded_manifest(
        self,
    ) -> None:
        releases = [
            lab_release("codex-lab-v0.0.0-lab.1", "2026-06-08T01:00:00Z"),
            lab_release("codex-lab-v0.0.0-lab.2", "2026-06-08T02:00:00Z"),
            lab_release("codex-lab-v0.0.0-lab.3", "2026-06-08T03:00:00Z", draft=True),
            lab_release("codex-v0.0.0", "2026-06-08T04:00:00Z"),
            lab_release(
                "codex-lab-v0.0.0-lab.4",
                "2026-06-08T05:00:00Z",
                asset_name="notes.txt",
            ),
        ]

        self.assertEqual(
            select_latest_lab_release_tag(releases), "codex-lab-v0.0.0-lab.2"
        )

    def test_select_latest_lab_release_tag_rejects_missing_manifest(self) -> None:
        with self.assertRaisesRegex(ValueError, "No published Codex Lab release"):
            select_latest_lab_release_tag(
                [
                    lab_release(
                        "codex-lab-v0.0.0-lab.1",
                        "2026-06-08T01:00:00Z",
                        asset_name="notes.txt",
                    )
                ]
            )

    def test_select_latest_lab_release_tag_includes_prereleases(self) -> None:
        releases = [
            lab_release(
                "codex-lab-v0.0.0-lab.2",
                "2026-06-08T02:00:00Z",
                prerelease=True,
            )
        ]

        self.assertEqual(
            select_latest_lab_release_tag(releases), "codex-lab-v0.0.0-lab.2"
        )

    def test_codex_lab_release_order_sorts_lab_tags_before_stable_tags(self) -> None:
        self.assertEqual(
            codex_lab_release_order("codex-lab-v1.2.3-lab.99"),
            (1, 2, 3, 0, 99),
        )
        self.assertEqual(codex_lab_release_order("codex-lab-v1.2.3"), (1, 2, 3, 1, 0))
        self.assertEqual(
            codex_lab_release_order("codex-lab-v1.2.4-lab.1"),
            (1, 2, 4, 0, 1),
        )
        self.assertIsNone(codex_lab_release_order("codex-lab-v1.2.3-custom.1"))

    def test_github_releases_url_quotes_repository_parts(self) -> None:
        self.assertEqual(
            github_releases_url("owner space/repo+name"),
            "https://api.github.com/repos/owner%20space/repo%2Bname/releases?per_page=100&page=1",
        )

    def test_github_releases_url_accepts_page(self) -> None:
        self.assertEqual(
            github_releases_url("example/repo", page=2),
            "https://api.github.com/repos/example/repo/releases?per_page=100&page=2",
        )

    def test_manifest_url_for_latest_release_uses_selected_tag(self) -> None:
        releases = [lab_release("codex-lab-v0.0.0-lab.2", "2026-06-08T02:00:00Z")]

        with mock.patch(
            "codex_lab_package.installer.download_json_url",
            return_value=releases,
        ):
            self.assertEqual(
                manifest_url_for_latest_release(repository="example/repo"),
                "https://github.com/example/repo/releases/download/codex-lab-v0.0.0-lab.2/codex-lab-distribution.json",
            )

    def test_latest_release_tag_scans_release_pages(self) -> None:
        calls: list[str] = []

        def fake_download(url: str) -> object:
            calls.append(url)
            if url.endswith("&page=1"):
                return [
                    lab_release("codex-v0.0.0", "2026-06-08T01:00:00Z"),
                    lab_release("codex-lab-v0.0.0-lab.1", "2026-06-08T02:00:00Z"),
                ]
            if url.endswith("&page=2"):
                return [lab_release("codex-lab-v0.0.0-lab.2", "2026-06-08T03:00:00Z")]
            if url.endswith("&page=3"):
                return []
            raise AssertionError(f"unexpected release page URL: {url}")

        with mock.patch(
            "codex_lab_package.installer.download_json_url",
            side_effect=fake_download,
        ):
            self.assertEqual(
                latest_release_tag(repository="example/repo"),
                "codex-lab-v0.0.0-lab.2",
            )

        self.assertEqual(
            calls,
            [
                "https://api.github.com/repos/example/repo/releases?per_page=100&page=1",
                "https://api.github.com/repos/example/repo/releases?per_page=100&page=2",
                "https://api.github.com/repos/example/repo/releases?per_page=100&page=3",
            ],
        )

    def test_latest_release_tag_rejects_non_list_release_page(self) -> None:
        with mock.patch(
            "codex_lab_package.installer.download_json_url",
            return_value={"message": "not a release list"},
        ):
            with self.assertRaisesRegex(ValueError, "response must be a list"):
                latest_release_tag(repository="example/repo")

    def test_installs_verified_release_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            install_root = root / "install"
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=install_root / "Custom Codex Lab.app",
                shim_dir=install_root / "bin",
                state_path=install_root / "state" / "install-state.json",
                download=release.download,
            )

            self.assertEqual(result.version, "1.2.3")
            self.assertEqual(result.release_tag, "codex-lab-v1.2.3-lab.1")
            self.assertEqual(
                result.app_dir,
                install_root.resolve(strict=False) / "Custom Codex Lab.app",
            )
            self.assertEqual(
                result.shim_path,
                (install_root / "bin").resolve(strict=False) / "codex-lab",
            )

            assert result.shim_path is not None
            shim = result.shim_path.read_text(encoding="utf-8")
            self.assertIn(str(result.app_dir), shim)
            completed = subprocess.run(
                [str(result.shim_path), "-c", "printf installed-ok"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.stdout, "installed-ok")

            state = json.loads(result.state_path.read_text(encoding="utf-8"))
            self.assertEqual(state["appPath"], str(result.app_dir))
            self.assertEqual(state["releaseTag"], "codex-lab-v1.2.3-lab.1")
            self.assertEqual(state["shimPath"], str(result.shim_path))
            self.assertEqual(state["version"], "1.2.3")

    def test_reads_install_state(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )

            status = read_install_state(result.state_path)

            self.assertEqual(status.app_path, result.app_dir)
            self.assertEqual(status.bundle_version, "42")
            self.assertEqual(status.release_tag, "codex-lab-v1.2.3-lab.1")
            self.assertEqual(status.shim_path, result.shim_path)
            self.assertEqual(status.source_commit, "abc123")
            self.assertEqual(status.state_path, result.state_path)
            self.assertEqual(status.version, "1.2.3")

    def test_status_command_prints_install_state(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )

            completed = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).resolve().parents[1] / "install_codex_lab.py"),
                    "--status",
                    "--state-path",
                    str(result.state_path),
                ],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            )

            self.assertEqual(
                completed.stdout,
                f"Codex Lab 1.2.3 from codex-lab-v1.2.3-lab.1\n"
                "Bundle version: 42\n"
                "Source commit: abc123\n"
                f"App: {result.app_dir}\n"
                f"Shim: {result.shim_path}\n"
                f"State: {result.state_path}\n",
            )

    def test_status_command_reports_missing_install_state(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            state_path = Path(temp_dir) / "missing-state.json"
            resolved_state_path = (
                state_path.parent.resolve(strict=False) / state_path.name
            )

            for command in ["--status", "--check", "--update"]:
                with self.subTest(command=command):
                    completed = subprocess.run(
                        [
                            sys.executable,
                            str(
                                Path(__file__).resolve().parents[1]
                                / "install_codex_lab.py"
                            ),
                            command,
                            "--state-path",
                            str(state_path),
                        ],
                        stderr=subprocess.PIPE,
                        stdout=subprocess.PIPE,
                        text=True,
                    )

                    self.assertEqual(completed.returncode, 1)
                    self.assertEqual(completed.stdout, "")
                    self.assertEqual(
                        completed.stderr,
                        f"Codex Lab install state not found: {resolved_state_path}\n",
                    )

    def test_check_command_reports_current_install(self) -> None:
        check = SimpleNamespace(
            installed=SimpleNamespace(release_tag="codex-lab-v1.2.3-lab.1"),
            latest_release_tag="codex-lab-v1.2.3-lab.1",
            update_available=False,
        )

        with mock.patch.object(
            install_codex_lab_cli, "check_for_update", return_value=check
        ):
            exit_code, stdout, stderr = run_install_cli("--check")

        self.assertEqual(exit_code, 0)
        self.assertEqual(stdout, "Codex Lab is up to date: codex-lab-v1.2.3-lab.1\n")
        self.assertEqual(stderr, "")

    def test_check_command_reports_available_update(self) -> None:
        check = SimpleNamespace(
            installed=SimpleNamespace(release_tag="codex-lab-v1.2.3-lab.1"),
            latest_release_tag="codex-lab-v1.2.4-lab.1",
            update_available=True,
        )

        with mock.patch.object(
            install_codex_lab_cli, "check_for_update", return_value=check
        ):
            exit_code, stdout, stderr = run_install_cli("--check")

        self.assertEqual(exit_code, 0)
        self.assertEqual(
            stdout,
            "Codex Lab update available: "
            "codex-lab-v1.2.3-lab.1 -> codex-lab-v1.2.4-lab.1\n",
        )
        self.assertEqual(stderr, "")

    def test_update_command_skips_current_install(self) -> None:
        update = SimpleNamespace(
            check=SimpleNamespace(
                installed=SimpleNamespace(release_tag="codex-lab-v1.2.3-lab.1")
            ),
            install=None,
        )

        with mock.patch.object(
            install_codex_lab_cli, "update_from_latest_release", return_value=update
        ):
            exit_code, stdout, stderr = run_install_cli("--update")

        self.assertEqual(exit_code, 0)
        self.assertEqual(stdout, "Codex Lab is up to date: codex-lab-v1.2.3-lab.1\n")
        self.assertEqual(stderr, "")

    def test_update_command_reports_installed_update(self) -> None:
        update = SimpleNamespace(
            check=SimpleNamespace(
                installed=SimpleNamespace(release_tag="codex-lab-v1.2.3-lab.1")
            ),
            install=SimpleNamespace(
                app_dir=Path("/tmp/Codex Lab.app"),
                release_tag="codex-lab-v1.2.4-lab.1",
                shim_path=Path("/tmp/bin/codex-lab"),
                state_path=Path("/tmp/state.json"),
            ),
        )

        with mock.patch.object(
            install_codex_lab_cli, "update_from_latest_release", return_value=update
        ):
            exit_code, stdout, stderr = run_install_cli("--update")

        self.assertEqual(exit_code, 0)
        self.assertEqual(
            stdout,
            "Updated Codex Lab "
            "codex-lab-v1.2.3-lab.1 -> codex-lab-v1.2.4-lab.1\n"
            "App: /tmp/Codex Lab.app\n"
            "Shim: /tmp/bin/codex-lab\n"
            "State: /tmp/state.json\n",
        )
        self.assertEqual(stderr, "")

    def test_update_command_reports_bad_zip_failure(self) -> None:
        with mock.patch.object(
            install_codex_lab_cli,
            "update_from_latest_release",
            side_effect=BadZipFile("File is not a zip file"),
        ):
            exit_code, stdout, stderr = run_install_cli("--update")

        self.assertEqual(exit_code, 1)
        self.assertEqual(stdout, "")
        self.assertEqual(
            stderr,
            "Could not update Codex Lab: File is not a zip file\n",
        )
        self.assertNotIn("Traceback", stderr)

    def test_install_command_reports_invalid_manifest_url(self) -> None:
        exit_code, stdout, stderr = run_install_cli(
            "--manifest-url", "http://example.test/manifest.json"
        )

        self.assertEqual(exit_code, 1)
        self.assertEqual(stdout, "")
        self.assertEqual(
            stderr,
            "Could not install Codex Lab: manifest URL must be an HTTPS URL: "
            "http://example.test/manifest.json\n",
        )
        self.assertNotIn("Traceback", stderr)

    def test_install_command_reports_release_tag_install_failure(self) -> None:
        with mock.patch.object(
            install_codex_lab_cli,
            "install_from_manifest_url",
            side_effect=OSError("download failed"),
        ):
            exit_code, stdout, stderr = run_install_cli(
                "--release-tag", "codex-lab-v1.2.3-lab.1"
            )

        self.assertEqual(exit_code, 1)
        self.assertEqual(stdout, "")
        self.assertEqual(stderr, "Could not install Codex Lab: download failed\n")
        self.assertNotIn("Traceback", stderr)

    def test_install_command_reports_latest_resolution_failure(self) -> None:
        with mock.patch.object(
            install_codex_lab_cli,
            "manifest_url_for_latest_release",
            side_effect=ValueError("No published Codex Lab release found"),
        ):
            exit_code, stdout, stderr = run_install_cli("--latest")

        self.assertEqual(exit_code, 1)
        self.assertEqual(stdout, "")
        self.assertEqual(
            stderr,
            "Could not install Codex Lab: No published Codex Lab release found\n",
        )
        self.assertNotIn("Traceback", stderr)

    def test_install_command_reports_bad_zip_failure(self) -> None:
        with mock.patch.object(
            install_codex_lab_cli,
            "install_from_manifest_url",
            side_effect=BadZipFile("File is not a zip file"),
        ):
            exit_code, stdout, stderr = run_install_cli(
                "--release-tag", "codex-lab-v1.2.3-lab.1"
            )

        self.assertEqual(exit_code, 1)
        self.assertEqual(stdout, "")
        self.assertEqual(
            stderr,
            "Could not install Codex Lab: File is not a zip file\n",
        )
        self.assertNotIn("Traceback", stderr)

    def test_check_for_update_reports_current_install(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries",
                return_value=[lab_release_summary("codex-lab-v1.2.3-lab.1")],
            ):
                check = check_for_update(state_path=result.state_path)

            self.assertFalse(check.update_available)
            self.assertEqual(check.installed.release_tag, "codex-lab-v1.2.3-lab.1")
            self.assertEqual(check.latest_release_tag, "codex-lab-v1.2.3-lab.1")

    def test_check_for_update_reports_available_update(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries",
                return_value=[
                    lab_release_summary("codex-lab-v1.2.3-lab.1"),
                    lab_release_summary(
                        "codex-lab-v1.2.4-lab.1",
                        published_at="2026-06-08T02:00:00Z",
                    ),
                ],
            ):
                check = check_for_update(state_path=result.state_path)

            self.assertTrue(check.update_available)
            self.assertEqual(check.installed.release_tag, "codex-lab-v1.2.3-lab.1")
            self.assertEqual(check.latest_release_tag, "codex-lab-v1.2.4-lab.1")

    def test_check_for_update_does_not_downgrade_to_later_published_lower_tag(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(
                root,
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
                bundle_version="43",
                commit="def456",
            )
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries",
                return_value=[
                    lab_release_summary("codex-lab-v1.2.4-lab.1"),
                    lab_release_summary(
                        "codex-lab-v1.2.3-lab.99",
                        published_at="2026-06-08T02:00:00Z",
                    ),
                ],
            ):
                check = check_for_update(state_path=result.state_path)

            self.assertFalse(check.update_available)
            self.assertEqual(check.installed.release_tag, "codex-lab-v1.2.4-lab.1")
            self.assertEqual(check.latest_release_tag, "codex-lab-v1.2.4-lab.1")

    def test_check_for_update_chooses_highest_ordered_tag_over_later_published_tag(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries",
                return_value=[
                    lab_release_summary("codex-lab-v1.2.3-lab.1"),
                    lab_release_summary(
                        "codex-lab-v1.2.4-lab.1",
                        published_at="2026-06-08T01:30:00Z",
                    ),
                    lab_release_summary(
                        "codex-lab-v1.2.3-lab.99",
                        published_at="2026-06-08T02:00:00Z",
                    ),
                ],
            ):
                check = check_for_update(state_path=result.state_path)

            self.assertTrue(check.update_available)
            self.assertEqual(check.installed.release_tag, "codex-lab-v1.2.3-lab.1")
            self.assertEqual(check.latest_release_tag, "codex-lab-v1.2.4-lab.1")

    def test_check_for_update_rejects_unpublished_installed_tag(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries",
                return_value=[lab_release_summary("codex-lab-v1.2.4-lab.1")],
            ):
                with self.assertRaisesRegex(
                    CodexLabUpdateError, "not in the published"
                ):
                    check_for_update(state_path=result.state_path)

    def test_update_from_latest_release_skips_current_install(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries",
                return_value=[lab_release_summary("codex-lab-v1.2.3-lab.1")],
            ):
                update = update_from_latest_release(
                    state_path=result.state_path,
                    download=mock.Mock(),
                )

            self.assertFalse(update.check.update_available)
            self.assertIsNone(update.install)

    def test_update_from_latest_release_preserves_recorded_install_layout(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            old_root = root / "old-release"
            new_root = root / "new-release"
            old_root.mkdir()
            new_root.mkdir()
            old_release = build_test_release(old_root)
            new_release = build_test_release(
                new_root,
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
                bundle_version="43",
                commit="def456",
            )
            app_dir = root / "custom" / "Apps" / "Codex Lab.app"
            shim_dir = root / "custom" / "bin"
            state_path = root / "custom" / "state" / "install-state.json"
            install_from_manifest_url(
                old_release.manifest_url,
                app_dir=app_dir,
                shim_dir=shim_dir,
                state_path=state_path,
                download=old_release.download,
            )

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries",
                return_value=[
                    lab_release_summary("codex-lab-v1.2.3-lab.1"),
                    lab_release_summary(
                        "codex-lab-v1.2.4-lab.1",
                        published_at="2026-06-08T02:00:00Z",
                    ),
                ],
            ):
                update = update_from_latest_release(
                    state_path=state_path,
                    download=new_release.download,
                )

            assert update.install is not None
            self.assertTrue(update.check.update_available)
            self.assertEqual(
                update.check.installed.release_tag, "codex-lab-v1.2.3-lab.1"
            )
            self.assertEqual(update.install.app_dir, app_dir.resolve(strict=False))
            self.assertEqual(update.install.release_tag, "codex-lab-v1.2.4-lab.1")
            self.assertEqual(
                update.install.shim_path, shim_dir.resolve(strict=False) / "codex-lab"
            )
            self.assertEqual(
                update.install.state_path, state_path.resolve(strict=False)
            )
            self.assertEqual(update.install.version, "1.2.4")

            status = read_install_state(state_path)
            self.assertEqual(status.app_path, app_dir.resolve(strict=False))
            self.assertEqual(status.bundle_version, "43")
            self.assertEqual(status.release_tag, "codex-lab-v1.2.4-lab.1")
            self.assertEqual(
                status.shim_path, shim_dir.resolve(strict=False) / "codex-lab"
            )
            self.assertEqual(status.source_commit, "def456")
            self.assertEqual(status.version, "1.2.4")

    def test_update_from_latest_release_preserves_no_shim_install(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            old_root = root / "old-release"
            new_root = root / "new-release"
            old_root.mkdir()
            new_root.mkdir()
            old_release = build_test_release(old_root)
            new_release = build_test_release(
                new_root,
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
                bundle_version="43",
                commit="def456",
            )
            app_dir = root / "custom" / "Apps" / "Codex Lab.app"
            state_path = root / "custom" / "state" / "install-state.json"
            install_from_manifest_url(
                old_release.manifest_url,
                app_dir=app_dir,
                shim_dir=None,
                state_path=state_path,
                download=old_release.download,
            )

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries",
                return_value=[
                    lab_release_summary("codex-lab-v1.2.3-lab.1"),
                    lab_release_summary(
                        "codex-lab-v1.2.4-lab.1",
                        published_at="2026-06-08T02:00:00Z",
                    ),
                ],
            ):
                update = update_from_latest_release(
                    state_path=state_path,
                    download=new_release.download,
                )

            assert update.install is not None
            self.assertEqual(update.install.app_dir, app_dir.resolve(strict=False))
            self.assertEqual(update.install.release_tag, "codex-lab-v1.2.4-lab.1")
            self.assertIsNone(update.install.shim_path)

            status = read_install_state(state_path)
            self.assertEqual(status.release_tag, "codex-lab-v1.2.4-lab.1")
            self.assertIsNone(status.shim_path)

    def test_rejects_artifact_url_that_is_not_manifest_sibling(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            manifest = release.manifest.copy()
            artifacts = {
                role: entry.copy() for role, entry in manifest["artifacts"].items()
            }
            manifest["artifacts"] = artifacts
            artifacts["appZip"]["downloadUrl"] = artifacts["appZip"][
                "downloadUrl"
            ].replace(
                "cbusillo/codex",
                "evil/example",
            )
            release.manifest = manifest

            with self.assertRaisesRegex(ValueError, "artifact URL does not match"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=root / "install" / "Codex Lab.app",
                    shim_dir=root / "install" / "bin",
                    state_path=root / "install" / "state.json",
                    download=release.download,
                )

    def test_rejects_checksum_drift_before_installing(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            subprocess.run(
                ["/usr/bin/tee", str(release.dist_dir / APP_ZIP)],
                input="not the signed artifact",
                check=True,
                stdout=subprocess.DEVNULL,
                text=True,
            )

            with self.assertRaisesRegex(ValueError, "sizeBytes does not match"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=root / "install" / "Codex Lab.app",
                    shim_dir=root / "install" / "bin",
                    state_path=root / "install" / "state.json",
                    download=release.download,
                )
            self.assertFalse((root / "install").exists())

    def test_rejects_unsafe_zip_member(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            with ZipFile(release.dist_dir / SHIM_ZIP, "w") as archive:
                archive.writestr("../codex-lab", "bad")
            update_manifest_artifact(release, "shimZip", SHIM_ZIP)
            write_file(
                release.dist_dir / "SHA256SUMS",
                f"{sha256_file(release.dist_dir / APP_ZIP)}  {APP_ZIP}\n"
                f"{sha256_file(release.dist_dir / SHIM_ZIP)}  {SHIM_ZIP}\n",
            )

            with self.assertRaisesRegex(ValueError, "Unsafe zip member"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=root / "install" / "Codex Lab.app",
                    shim_dir=root / "install" / "bin",
                    state_path=root / "install" / "state.json",
                    download=release.download,
                )

    def test_refuses_to_replace_without_force(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            app_dir = root / "install" / "Codex Lab.app"
            app_dir.mkdir(parents=True)

            with self.assertRaisesRegex(FileExistsError, "already exists"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=app_dir,
                    shim_dir=root / "install" / "bin",
                    state_path=root / "install" / "state.json",
                    download=release.download,
                )

    def test_rejects_symlink_install_target_before_resolving_it(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            real_target = root / "real-target"
            real_target.mkdir()
            symlink_target = root / "install" / "Codex Lab.app"
            symlink_target.parent.mkdir()
            symlink_target.symlink_to(real_target)

            with self.assertRaisesRegex(ValueError, "must not be a symlink"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=symlink_target,
                    shim_dir=root / "install" / "bin",
                    state_path=root / "install" / "state.json",
                    force=True,
                    download=release.download,
                )

            self.assertTrue(symlink_target.is_symlink())
            self.assertTrue(real_target.is_dir())

    def test_resolves_symlink_app_parent_before_installing(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            real_apps = root / "real-apps"
            real_apps.mkdir()
            app_parent = root / "install" / "Apps"
            app_parent.parent.mkdir()
            app_parent.symlink_to(real_apps)

            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=app_parent / "Codex Lab.app",
                shim_dir=None,
                state_path=root / "install" / "state.json",
                force=True,
                download=release.download,
            )

            self.assertTrue(app_parent.is_symlink())
            self.assertEqual(
                result.app_dir, real_apps.resolve(strict=False) / "Codex Lab.app"
            )
            self.assertTrue((real_apps / "Codex Lab.app").is_dir())

    def test_resolves_symlink_app_ancestor_before_installing(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            real_install = root / "real-install"
            real_install.mkdir()
            install_link = root / "install"
            install_link.symlink_to(real_install)

            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=install_link / "Apps" / "Codex Lab.app",
                shim_dir=None,
                state_path=root / "state" / "install-state.json",
                force=True,
                download=release.download,
            )

            self.assertEqual(
                result.app_dir,
                (real_install / "Apps").resolve(strict=False) / "Codex Lab.app",
            )
            self.assertTrue((real_install / "Apps" / "Codex Lab.app").is_dir())

    def test_resolves_symlink_shim_parent_before_installing(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            real_bin = root / "real-bin"
            real_bin.mkdir()
            shim_dir = root / "install" / "bin"
            shim_dir.parent.mkdir()
            shim_dir.symlink_to(real_bin)

            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=shim_dir,
                state_path=root / "install" / "state.json",
                force=True,
                download=release.download,
            )

            self.assertTrue(shim_dir.is_symlink())
            self.assertEqual(
                result.shim_path, real_bin.resolve(strict=False) / "codex-lab"
            )
            self.assertTrue((real_bin / "codex-lab").is_file())

    def test_resolves_symlink_state_parent_before_writing_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            real_state = root / "real-state"
            real_state.mkdir()
            state_parent = root / "install" / "state"
            state_parent.parent.mkdir()
            state_parent.symlink_to(real_state)

            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=None,
                state_path=state_parent / "install-state.json",
                force=True,
                download=release.download,
            )

            self.assertTrue(state_parent.is_symlink())
            self.assertEqual(
                result.state_path,
                real_state.resolve(strict=False) / "install-state.json",
            )
            self.assertTrue((real_state / "install-state.json").is_file())

    def test_preflights_all_targets_before_installing(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            app_dir = root / "install" / "Codex Lab.app"
            shim_dir = root / "install" / "bin"
            shim_dir.mkdir(parents=True)
            write_file(shim_dir / "codex-lab", "#!/bin/sh\n")

            with self.assertRaisesRegex(FileExistsError, "already exists"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=app_dir,
                    shim_dir=shim_dir,
                    state_path=root / "install" / "state.json",
                    download=release.download,
                )
            self.assertFalse(app_dir.exists())

    def test_rolls_back_replacements_when_state_write_fails(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            install_root = root / "install"
            app_dir = install_root / "Codex Lab.app"
            shim_dir = install_root / "bin"
            app_dir.mkdir(parents=True)
            shim_dir.mkdir(parents=True)
            write_file(app_dir / "old-app-marker", "old app")
            write_file(shim_dir / "codex-lab", "old shim")
            write_file(install_root / "state-parent", "not a directory")

            with self.assertRaises(OSError):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=app_dir,
                    shim_dir=shim_dir,
                    state_path=install_root / "state-parent" / "state.json",
                    force=True,
                    download=release.download,
                )

            self.assertTrue((app_dir / "old-app-marker").is_file())
            self.assertEqual(
                (shim_dir / "codex-lab").read_text(encoding="utf-8"), "old shim"
            )

    def test_replace_path_restores_backup_after_partial_move_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source = root / "new-app"
            target = root / "Codex Lab.app"
            source.mkdir()
            target.mkdir()
            write_file(source / "new-marker", "new app")
            write_file(target / "old-marker", "old app")
            real_move = shutil.move
            calls = []

            def move_with_partial_failure(src: str, dst: str) -> str:
                calls.append((src, dst))
                if len(calls) == 1:
                    partial_target = Path(dst)
                    partial_target.mkdir(parents=True, exist_ok=True)
                    write_file(partial_target / "partial-marker", "partial app")
                    raise OSError("copy failed")
                return real_move(src, dst)

            with mock.patch(
                "codex_lab_package.installer.shutil.move",
                side_effect=move_with_partial_failure,
            ):
                with self.assertRaisesRegex(OSError, "copy failed"):
                    replace_path(source, target, force=True)

            self.assertTrue((target / "old-marker").is_file())
            self.assertFalse((target / "partial-marker").exists())


class TestRelease:
    def __init__(self, dist_dir: Path, manifest_url: str, manifest: dict) -> None:
        self.dist_dir = dist_dir
        self.manifest_url = manifest_url
        self.manifest = manifest

    def download(self, url: str, dest: Path) -> None:
        file_name = url.rsplit("/", 1)[-1]
        dest.parent.mkdir(parents=True, exist_ok=True)
        if file_name == MANIFEST_NAME:
            subprocess.run(
                ["/usr/bin/tee", str(dest)],
                input=json.dumps(self.manifest, indent=2, sort_keys=True) + "\n",
                check=True,
                stdout=subprocess.DEVNULL,
                text=True,
            )
            return
        shutil.copyfile(self.dist_dir / file_name, dest)


class FakeHttpResponse:
    def __init__(self, body: bytes) -> None:
        self.body = body
        self.offset = 0

    def __enter__(self) -> "FakeHttpResponse":
        return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> None:
        return None

    def read(self, size: int = -1) -> bytes:
        if size is None or size < 0:
            size = len(self.body) - self.offset
        chunk = self.body[self.offset : self.offset + size]
        self.offset += len(chunk)
        return chunk


def lab_release(
    tag_name: str,
    published_at: str,
    *,
    asset_name: str = MANIFEST_NAME,
    draft: bool = False,
    prerelease: bool = True,
) -> dict:
    return {
        "assets": [
            {
                "name": asset_name,
                "state": "uploaded",
            }
        ],
        "draft": draft,
        "published_at": published_at,
        "prerelease": prerelease,
        "tag_name": tag_name,
    }


def lab_release_summary(
    tag_name: str,
    *,
    published_at: str = "2026-06-08T01:00:00Z",
) -> CodexLabReleaseSummary:
    return CodexLabReleaseSummary(published_at=published_at, tag_name=tag_name)


def run_install_cli(*args: str) -> tuple[int, str, str]:
    stdout = io.StringIO()
    stderr = io.StringIO()
    with (
        mock.patch.object(sys, "argv", ["install_codex_lab.py", *args]),
        contextlib.redirect_stdout(stdout),
        contextlib.redirect_stderr(stderr),
    ):
        exit_code = install_codex_lab_cli.main()
    return exit_code, stdout.getvalue(), stderr.getvalue()


def build_test_release(
    root: Path,
    *,
    release_tag: str = "codex-lab-v1.2.3-lab.1",
    version: str = "1.2.3",
    bundle_version: str = "42",
    commit: str = "abc123",
) -> TestRelease:
    build_root = root / "build"
    dist_dir = root / "dist"
    dist_dir.mkdir()
    codex_bin = root / "fake-codex"
    write_file(codex_bin, '#!/bin/sh\nexec /bin/sh "$@"\n')
    os.chmod(codex_bin, 0o755)
    result = build_codex_lab_app(
        CodexLabAppOptions(
            app_dir=build_root / "Codex Lab.app",
            codex_bin=codex_bin,
            shim_dir=build_root / "bin",
            short_version=version,
            bundle_version=bundle_version,
        )
    )
    assert result.shim_path is not None
    zip_tree(result.app_dir, dist_dir / APP_ZIP)
    zip_tree(result.shim_path, dist_dir / SHIM_ZIP, arcname=Path("bin/codex-lab"))
    write_file(
        dist_dir / "SHA256SUMS",
        f"{sha256_file(dist_dir / APP_ZIP)}  {APP_ZIP}\n"
        f"{sha256_file(dist_dir / SHIM_ZIP)}  {SHIM_ZIP}\n",
    )

    manifest_url = manifest_url_for_release_tag(release_tag)
    manifest = build_manifest(
        dist_dir=dist_dir,
        checksums={
            APP_ZIP: sha256_file(dist_dir / APP_ZIP),
            SHIM_ZIP: sha256_file(dist_dir / SHIM_ZIP),
        },
        version=version,
        bundle_version=bundle_version,
        commit=commit,
        repository="cbusillo/codex",
        workflow="codex-lab-release",
        run_id="100",
        run_attempt="1",
        release_tag=release_tag,
        download_base_url=manifest_url.rsplit("/", 1)[0],
        generated_at="2026-06-07T00:00:00Z",
    )
    return TestRelease(dist_dir, manifest_url, manifest)


def update_manifest_artifact(release: TestRelease, role: str, file_name: str) -> None:
    artifacts = {
        role: entry.copy() for role, entry in release.manifest["artifacts"].items()
    }
    release.manifest = {**release.manifest, "artifacts": artifacts}
    artifacts[role]["sha256"] = sha256_file(release.dist_dir / file_name)
    artifacts[role]["sizeBytes"] = (release.dist_dir / file_name).stat().st_size


def zip_tree(source: Path, dest: Path, *, arcname: Path | None = None) -> None:
    arcname = arcname or Path(source.name)
    with ZipFile(dest, "w", compression=ZIP_DEFLATED) as archive:
        if source.is_dir():
            write_zip_entry(archive, source, arcname)
            for child in sorted(source.rglob("*")):
                write_zip_entry(archive, child, arcname / child.relative_to(source))
        else:
            write_zip_entry(archive, source, arcname)


def write_zip_entry(archive: ZipFile, source: Path, arcname: Path) -> None:
    name = str(arcname)
    mode = source.stat().st_mode
    if source.is_dir():
        info = ZipInfo(f"{name}/")
        info.external_attr = ((stat.S_IFDIR | 0o755) & 0xFFFF) << 16
        archive.writestr(info, b"")
        return
    info = ZipInfo(name)
    info.external_attr = ((stat.S_IFREG | stat.S_IMODE(mode)) & 0xFFFF) << 16
    archive.writestr(info, source.read_bytes())


def write_file(path: Path, value: str) -> None:
    subprocess.run(
        ["/usr/bin/tee", str(path)],
        input=value,
        check=True,
        stdout=subprocess.DEVNULL,
        text=True,
    )


if __name__ == "__main__":
    unittest.main()
