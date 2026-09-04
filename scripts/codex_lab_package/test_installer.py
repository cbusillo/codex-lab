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
import threading
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.distribution_manifest import APP_ZIP
from codex_lab_package.distribution_manifest import ENGINE_ZIP
from codex_lab_package.distribution_manifest import MANIFEST_NAME
from codex_lab_package.distribution_manifest import SHIM_ZIP
from codex_lab_package.distribution_manifest import build_manifest
from codex_lab_package.distribution_manifest import sha256_file
from codex_lab_package.distribution_manifest import sha256_zip_member
from codex_lab_package.installer import DOWNLOAD_TIMEOUT_SECONDS
from codex_lab_package.installer import CodexLabRollbackError
from codex_lab_package.installer import CodexLabReleaseSummary
from codex_lab_package.installer import CodexLabUpdateError
from codex_lab_package.installer import activate_code_route
from codex_lab_package.installer import check_for_update
from codex_lab_package.installer import codex_lab_release_order
from codex_lab_package.installer import deactivate_code_route
from codex_lab_package.installer import download_json_url
from codex_lab_package.installer import download_url
from codex_lab_package.installer import github_releases_url
from codex_lab_package.installer import install_from_manifest_url
from codex_lab_package.installer import EngineProvisioningOperations
from codex_lab_package.installer import latest_release_tag
from codex_lab_package.installer import manifest_url_for_latest_release
from codex_lab_package.installer import manifest_url_for_release_tag
from codex_lab_package.installer import read_install_state
from codex_lab_package.installer import replace_path
from codex_lab_package.installer import select_latest_lab_release_tag
from codex_lab_package.installer import update_from_latest_release
from codex_lab_package.installer import uninstall_codex_lab
from codex_lab_package.engine_contract import ENGINE_SIGNING_IDENTIFIER
from codex_lab_package.engine_contract import CODE_MODE_HOST_SIGNING_IDENTIFIER
from codex_lab_package.engine_contract import ENGINE_TEAM_IDENTIFIER
from codex_lab_package.layout import CodexLabAppOptions
from codex_lab_package.layout import build_codex_lab_app
from codex_lab_package.supervisor import CodeModeHostIdentity
from codex_lab_package.supervisor import EngineIdentity
from codex_lab_package.supervisor import SupervisorPaths
import codex_lab_package.installer as installer_module
import install_codex_lab as install_codex_lab_cli


class CodexLabInstallerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.supervisor_temp_dir = tempfile.TemporaryDirectory()
        supervisor_root = Path(self.supervisor_temp_dir.name).resolve()
        self.supervisor_paths = SupervisorPaths(
            lab_home=supervisor_root / "lab-home",
            launch_agents_dir=supervisor_root / "LaunchAgents",
        )
        self.supervisor_installs: list[tuple[SupervisorPaths, object]] = []
        self.supervisor_uninstalls: list[SupervisorPaths] = []
        engine_operations = EngineProvisioningOperations(
            inspect=fake_inspect_engine,
            install_supervisor=self._install_supervisor,
            uninstall_supervisor=self._uninstall_supervisor,
            inspect_code_mode_host=fake_inspect_code_mode_host,
        )
        self.default_paths_patch = mock.patch(
            "codex_lab_package.installer.default_supervisor_paths",
            return_value=self.supervisor_paths,
        )
        self.default_operations_patch = mock.patch(
            "codex_lab_package.installer.DEFAULT_ENGINE_OPERATIONS",
            engine_operations,
        )
        self.default_paths_patch.start()
        self.default_operations_patch.start()
        self.addCleanup(self.default_paths_patch.stop)
        self.addCleanup(self.default_operations_patch.stop)
        self.addCleanup(self.supervisor_temp_dir.cleanup)

    def _install_supervisor(self, paths: SupervisorPaths, release: object) -> None:
        self.supervisor_installs.append((paths, release))
        paths.runner.parent.mkdir(parents=True, exist_ok=True)
        paths.launch_agents_dir.mkdir(parents=True, exist_ok=True)
        write_file(paths.runner, "supervisor runner")
        write_file(paths.plist, "launch agent")

    def _uninstall_supervisor(self, paths: SupervisorPaths) -> None:
        self.supervisor_uninstalls.append(paths)
        paths.plist.unlink(missing_ok=True)
        shutil.rmtree(paths.supervisor_dir, ignore_errors=True)

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
            self.assertEqual(result.engine_path, self.supervisor_paths.managed_cli)
            self.assertTrue(result.engine_path.is_file())
            self.assertEqual(result.supervisor_label, self.supervisor_paths.label)
            self.assertEqual(len(self.supervisor_installs), 1)
            installed_release = self.supervisor_installs[0][1]
            self.assertEqual(
                installed_release.sha256,
                release.manifest["managedEngine"]["sha256"],
            )
            self.assertEqual(installed_release.source_commit, "abc123")
            self.assertEqual(installed_release.release_version, "1.2.3-lab.1")
            self.assertEqual(installed_release.version, "1.2.3")

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
            self.assertEqual(state["enginePath"], str(result.engine_path))
            self.assertEqual(state["labHome"], str(self.supervisor_paths.lab_home))
            self.assertEqual(
                state["launchAgentsDir"],
                str(self.supervisor_paths.launch_agents_dir),
            )
            self.assertEqual(state["listenHost"], self.supervisor_paths.listen_host)
            self.assertEqual(state["listenPort"], self.supervisor_paths.listen_port)
            self.assertEqual(state["releaseTag"], "codex-lab-v1.2.3-lab.1")
            self.assertEqual(state["releaseVersion"], "1.2.3-lab.1")
            self.assertEqual(state["shimPath"], str(result.shim_path))
            self.assertEqual(state["supervisorLabel"], self.supervisor_paths.label)
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
            self.assertEqual(status.engine_path, result.engine_path)
            self.assertEqual(status.lab_home, self.supervisor_paths.lab_home)
            self.assertEqual(
                status.launch_agents_dir,
                self.supervisor_paths.launch_agents_dir,
            )
            self.assertEqual(status.listen_host, self.supervisor_paths.listen_host)
            self.assertEqual(status.listen_port, self.supervisor_paths.listen_port)
            self.assertEqual(status.release_tag, "codex-lab-v1.2.3-lab.1")
            self.assertEqual(status.shim_path, result.shim_path)
            self.assertEqual(status.source_commit, "abc123")
            self.assertEqual(status.state_path, result.state_path)
            self.assertEqual(status.supervisor_label, self.supervisor_paths.label)
            self.assertEqual(status.version, "1.2.3")

    def test_reads_legacy_install_state_without_release_version(self) -> None:
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
            state = json.loads(result.state_path.read_text(encoding="utf-8"))
            state.pop("releaseVersion")
            state.pop("managedEngine")
            result.state_path.write_text(json.dumps(state), encoding="utf-8")

            status = read_install_state(result.state_path)

            self.assertEqual(status.release_version, "1.2.3-lab.1")
            uninstall_codex_lab(state_path=result.state_path)
            self.assertFalse(result.state_path.exists())

    def test_rejects_engine_release_version_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)

            def inspect_wrong_release_version(path: Path) -> EngineIdentity:
                identity = fake_inspect_engine(path)
                return EngineIdentity(
                    build_channel=identity.build_channel,
                    build_profile=identity.build_profile,
                    release_version="1.2.3-lab.99",
                    sha256=identity.sha256,
                    signing_identifier=identity.signing_identifier,
                    source_commit=identity.source_commit,
                    team_identifier=identity.team_identifier,
                    version=identity.version,
                )

            operations = EngineProvisioningOperations(
                inspect=inspect_wrong_release_version,
                install_supervisor=self._install_supervisor,
                uninstall_supervisor=self._uninstall_supervisor,
                inspect_code_mode_host=fake_inspect_code_mode_host,
            )
            with self.assertRaisesRegex(ValueError, "release version"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=root / "install" / "Codex Lab.app",
                    shim_dir=root / "install" / "bin",
                    state_path=root / "install" / "install-state.json",
                    download=release.download,
                    engine_operations=operations,
                )

            self.assertFalse((root / "install").exists())

    def test_active_code_route_refuses_install_and_update_before_download(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            release = build_test_release(root / "current")
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )
            route_path = root / "route" / "code"
            activate_code_route(
                state_path=result.state_path,
                code_route_path=route_path,
            )
            replacement = build_test_release(
                root / "replacement",
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
            )
            download = mock.Mock(side_effect=replacement.download)

            with self.assertRaisesRegex(
                ValueError, "Deactivate the explicit code route"
            ):
                install_from_manifest_url(
                    replacement.manifest_url,
                    app_dir=result.app_dir,
                    shim_dir=result.shim_path.parent if result.shim_path else None,
                    state_path=result.state_path,
                    force=True,
                    download=download,
                )
            download.assert_not_called()

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries"
            ) as releases:
                with self.assertRaisesRegex(
                    ValueError, "Deactivate the explicit code route"
                ):
                    update_from_latest_release(state_path=result.state_path)
            releases.assert_not_called()

    def test_install_serializes_with_code_route_activation(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            old_release = build_test_release(root / "old")
            new_release = build_test_release(
                root / "new",
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
                bundle_version="43",
                commit="def456",
            )
            result = install_from_manifest_url(
                old_release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=old_release.download,
            )
            install_started = threading.Event()
            allow_install = threading.Event()
            activation_done = threading.Event()
            errors: list[BaseException] = []

            def blocking_download(url: str, destination: Path) -> None:
                if not install_started.is_set():
                    install_started.set()
                    if not allow_install.wait(timeout=10):
                        raise TimeoutError("test install was not released")
                new_release.download(url, destination)

            def run_install() -> None:
                try:
                    install_from_manifest_url(
                        new_release.manifest_url,
                        app_dir=result.app_dir,
                        shim_dir=result.shim_path.parent
                        if result.shim_path is not None
                        else None,
                        state_path=result.state_path,
                        supervisor_paths=self.supervisor_paths,
                        force=True,
                        download=blocking_download,
                    )
                except BaseException as exc:
                    errors.append(exc)

            route_path = root / "route" / "code"

            def run_activation() -> None:
                try:
                    activate_code_route(
                        state_path=result.state_path,
                        code_route_path=route_path,
                    )
                except BaseException as exc:
                    errors.append(exc)
                finally:
                    activation_done.set()

            install_thread = threading.Thread(target=run_install)
            activation_thread = threading.Thread(target=run_activation)
            install_thread.start()
            self.assertTrue(install_started.wait(timeout=10))
            activation_thread.start()
            self.assertFalse(activation_done.wait(timeout=0.2))
            allow_install.set()
            install_thread.join(timeout=30)
            activation_thread.join(timeout=30)

            self.assertFalse(install_thread.is_alive())
            self.assertFalse(activation_thread.is_alive())
            self.assertEqual(errors, [])
            status = read_install_state(result.state_path)
            self.assertEqual(status.release_tag, new_release.manifest["release"]["tag"])
            self.assertIsNotNone(status.code_route)

    def test_uninstall_requires_route_deactivation(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )
            route_path = root / "route" / "code"
            route_path.parent.mkdir()
            route_path.write_text("prior code route", encoding="utf-8")
            route_path.chmod(0o751)
            activate_code_route(
                state_path=result.state_path,
                code_route_path=route_path,
            )

            with self.assertRaisesRegex(
                ValueError, "Deactivate the explicit code route"
            ):
                uninstall_codex_lab(state_path=result.state_path)

            self.assertTrue(result.state_path.exists())
            deactivate_code_route(
                state_path=result.state_path,
                code_route_path=route_path,
            )
            uninstalled = uninstall_codex_lab(state_path=result.state_path)

            self.assertIsNone(uninstalled.restored_code_route_path)
            self.assertEqual(route_path.read_text(encoding="utf-8"), "prior code route")
            self.assertEqual(route_path.stat().st_mode & 0o777, 0o751)
            self.assertFalse(result.state_path.exists())

    def test_uninstall_rejects_replaced_code_mode_host(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )
            assert result.code_mode_host_path is not None
            result.code_mode_host_path.write_text("replacement", encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "host identity"):
                uninstall_codex_lab(state_path=result.state_path)

            self.assertTrue(result.state_path.exists())
            self.assertTrue(result.app_dir.exists())

    def test_uninstall_cleanup_failure_is_best_effort(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            release = build_test_release(root)
            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=release.download,
            )
            real_remove_path = installer_module.remove_path

            def fail_backup_cleanup(path: Path) -> None:
                if ".codex-lab-backup-" in path.name:
                    raise OSError("cleanup failed")
                real_remove_path(path)

            with mock.patch(
                "codex_lab_package.installer.remove_path",
                side_effect=fail_backup_cleanup,
            ):
                uninstall_codex_lab(state_path=result.state_path)

            self.assertFalse(result.state_path.exists())
            self.assertFalse(result.app_dir.exists())

    def test_update_verifies_legacy_identity_from_published_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            old_release = build_test_release(root / "old")
            new_release = build_test_release(
                root / "new",
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
                bundle_version="43",
                commit="def456",
            )
            result = install_from_manifest_url(
                old_release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=old_release.download,
            )
            state = json.loads(result.state_path.read_text(encoding="utf-8"))
            state["managedEngine"] = None
            state["managedCodeModeHost"] = None
            result.state_path.write_text(
                json.dumps(state, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )

            def download(url: str, destination: Path) -> None:
                if f"/{old_release.manifest['release']['tag']}/" in url:
                    old_release.download(url, destination)
                else:
                    new_release.download(url, destination)

            with mock.patch(
                "codex_lab_package.installer.lab_distribution_release_summaries",
                return_value=[
                    CodexLabReleaseSummary(
                        published_at="2026-01-01T00:00:00Z",
                        tag_name=old_release.manifest["release"]["tag"],
                    ),
                    CodexLabReleaseSummary(
                        published_at="2026-01-02T00:00:00Z",
                        tag_name=new_release.manifest["release"]["tag"],
                    ),
                ],
            ):
                update = update_from_latest_release(
                    state_path=result.state_path,
                    download=download,
                )
            assert update.install is not None
            updated = update.install

            status = read_install_state(updated.state_path)
            self.assertEqual(status.release_tag, new_release.manifest["release"]["tag"])
            self.assertIsNotNone(status.engine_sha256)
            self.assertIsNotNone(status.code_mode_host_sha256)

    def test_downgrades_to_schema_two_release_and_removes_managed_host(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            current_release = build_test_release(
                root / "current",
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
            )
            rollback_release = build_test_release(root / "rollback")
            convert_test_release_to_schema_two(rollback_release)
            install_root = root / "install"
            self.supervisor_paths.code_mode_host.parent.mkdir(parents=True)
            write_file(self.supervisor_paths.code_mode_host, "user host")
            current = install_from_manifest_url(
                current_release.manifest_url,
                app_dir=install_root / "Codex Lab.app",
                shim_dir=install_root / "bin",
                state_path=install_root / "install-state.json",
                force=True,
                download=current_release.download,
            )
            self.assertTrue(self.supervisor_paths.code_mode_host.is_file())

            rolled_back = install_from_manifest_url(
                rollback_release.manifest_url,
                app_dir=current.app_dir,
                shim_dir=current.shim_path.parent if current.shim_path else None,
                state_path=current.state_path,
                supervisor_paths=self.supervisor_paths,
                force=True,
                download=rollback_release.download,
            )

            self.assertIsNone(rolled_back.code_mode_host_path)
            self.assertFalse(self.supervisor_paths.code_mode_host.exists())
            self.assertEqual(read_install_state(current.state_path).version, "1.2.3")
            uninstalled = uninstall_codex_lab(state_path=current.state_path)
            self.assertEqual(
                uninstalled.restored_code_mode_host_path,
                self.supervisor_paths.code_mode_host,
            )
            self.assertEqual(
                self.supervisor_paths.code_mode_host.read_text(encoding="utf-8"),
                "user host",
            )

    def test_rejects_unsigned_engine_before_installing(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)

            def reject_unsigned_engine(path: Path) -> EngineIdentity:
                raise ValueError(f"managed engine is unsigned: {path}")

            operations = EngineProvisioningOperations(
                inspect=reject_unsigned_engine,
                install_supervisor=self._install_supervisor,
                uninstall_supervisor=self._uninstall_supervisor,
            )
            with self.assertRaisesRegex(ValueError, "unsigned"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=root / "install" / "Codex Lab.app",
                    shim_dir=root / "install" / "bin",
                    state_path=root / "install" / "install-state.json",
                    download=release.download,
                    engine_operations=operations,
                )

            self.assertFalse((root / "install").exists())
            self.assertFalse(self.supervisor_paths.managed_cli.exists())

    def test_supervisor_failure_rolls_back_app_shim_engine_and_state(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            old_release = build_test_release(root / "old")
            new_release = build_test_release(
                root / "new",
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
                bundle_version="43",
                commit="def456",
            )
            install_root = root / "install"
            old_result = install_from_manifest_url(
                old_release.manifest_url,
                app_dir=install_root / "Codex Lab.app",
                shim_dir=install_root / "bin",
                state_path=install_root / "install-state.json",
                download=old_release.download,
            )
            old_engine = old_result.engine_path.read_bytes()
            old_state = old_result.state_path.read_bytes()
            assert old_result.shim_path is not None
            old_shim = old_result.shim_path.read_bytes()
            write_file(old_result.app_dir / "old-marker", "old app")

            def fail_supervisor(
                paths: SupervisorPaths,
                release: object,
            ) -> None:
                raise RuntimeError(f"supervisor failed for {paths.label}: {release}")

            operations = EngineProvisioningOperations(
                inspect=fake_inspect_engine,
                install_supervisor=fail_supervisor,
                uninstall_supervisor=self._uninstall_supervisor,
                inspect_code_mode_host=fake_inspect_code_mode_host,
            )
            with self.assertRaisesRegex(RuntimeError, "supervisor failed"):
                install_from_manifest_url(
                    new_release.manifest_url,
                    app_dir=old_result.app_dir,
                    shim_dir=old_result.shim_path.parent,
                    state_path=old_result.state_path,
                    supervisor_paths=self.supervisor_paths,
                    force=True,
                    download=new_release.download,
                    engine_operations=operations,
                )

            self.assertTrue((old_result.app_dir / "old-marker").is_file())
            self.assertEqual(old_result.shim_path.read_bytes(), old_shim)
            self.assertEqual(old_result.engine_path.read_bytes(), old_engine)
            self.assertEqual(old_result.state_path.read_bytes(), old_state)

    def test_force_update_refuses_recorded_unmanaged_app_target(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            old_release = build_test_release(root / "old")
            new_release = build_test_release(
                root / "new",
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
                bundle_version="43",
                commit="def456",
            )
            old_result = install_from_manifest_url(
                old_release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                download=old_release.download,
            )
            old_engine = old_result.engine_path.read_bytes()
            old_state = old_result.state_path.read_bytes()
            shutil.rmtree(old_result.app_dir)
            old_result.app_dir.mkdir()
            write_file(old_result.app_dir / "unmanaged-marker", "do not replace")

            with self.assertRaisesRegex(ValueError, "not a managed Codex Lab"):
                install_from_manifest_url(
                    new_release.manifest_url,
                    app_dir=old_result.app_dir,
                    shim_dir=old_result.shim_path.parent,
                    state_path=old_result.state_path,
                    supervisor_paths=self.supervisor_paths,
                    force=True,
                    download=new_release.download,
                )

            self.assertTrue((old_result.app_dir / "unmanaged-marker").is_file())
            self.assertEqual(old_result.engine_path.read_bytes(), old_engine)
            self.assertEqual(old_result.state_path.read_bytes(), old_state)
            self.assertEqual(len(self.supervisor_installs), 1)

    def test_reports_file_rollback_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)

            def fail_supervisor(
                paths: SupervisorPaths,
                managed_engine: object,
            ) -> None:
                raise RuntimeError(
                    f"supervisor provisioning failed: {paths.label} {managed_engine}"
                )

            operations = EngineProvisioningOperations(
                inspect=fake_inspect_engine,
                install_supervisor=fail_supervisor,
                uninstall_supervisor=self._uninstall_supervisor,
                inspect_code_mode_host=fake_inspect_code_mode_host,
            )
            with (
                mock.patch(
                    "codex_lab_package.installer.rollback_replacements",
                    side_effect=OSError("rollback exploded"),
                ),
                self.assertRaisesRegex(
                    CodexLabRollbackError,
                    "rollback did not complete: rollback exploded",
                ) as raised,
            ):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=root / "install" / "Codex Lab.app",
                    shim_dir=root / "install" / "bin",
                    state_path=root / "install" / "install-state.json",
                    download=release.download,
                    engine_operations=operations,
                )

            self.assertIsInstance(raised.exception.__cause__, RuntimeError)

    def test_uninstall_restores_preinstaller_engine(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            prior_engine = b"prior managed engine"
            self.supervisor_paths.managed_cli.parent.mkdir(parents=True)
            self.supervisor_paths.managed_cli.write_bytes(prior_engine)
            os.chmod(self.supervisor_paths.managed_cli, 0o755)

            result = install_from_manifest_url(
                release.manifest_url,
                app_dir=root / "install" / "Codex Lab.app",
                shim_dir=root / "install" / "bin",
                state_path=root / "install" / "install-state.json",
                force=True,
                download=release.download,
            )
            status = read_install_state(result.state_path)
            assert status.engine_backup_path is not None
            self.assertEqual(status.engine_backup_path.read_bytes(), prior_engine)

            uninstall = uninstall_codex_lab(state_path=result.state_path)

            self.assertFalse(result.app_dir.exists())
            assert result.shim_path is not None
            self.assertFalse(result.shim_path.exists())
            self.assertFalse(result.state_path.exists())
            self.assertEqual(result.engine_path.read_bytes(), prior_engine)
            self.assertEqual(uninstall.restored_engine_path, result.engine_path)
            self.assertEqual(self.supervisor_uninstalls, [self.supervisor_paths])
            self.assertFalse(self.supervisor_paths.runner.exists())
            self.assertFalse(self.supervisor_paths.plist.exists())

    def test_uninstall_failure_restores_install_and_supervisor(self) -> None:
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
            installed_engine = result.engine_path.read_bytes()

            def fail_uninstall(paths: SupervisorPaths) -> None:
                raise RuntimeError(f"could not stop {paths.label}")

            operations = EngineProvisioningOperations(
                inspect=fake_inspect_engine,
                install_supervisor=self._install_supervisor,
                uninstall_supervisor=fail_uninstall,
                inspect_code_mode_host=fake_inspect_code_mode_host,
            )
            with self.assertRaisesRegex(RuntimeError, "could not stop"):
                uninstall_codex_lab(
                    state_path=result.state_path,
                    engine_operations=operations,
                )

            self.assertTrue(result.app_dir.is_dir())
            assert result.shim_path is not None
            self.assertTrue(result.shim_path.is_file())
            self.assertTrue(result.state_path.is_file())
            self.assertEqual(result.engine_path.read_bytes(), installed_engine)
            self.assertEqual(len(self.supervisor_installs), 2)
            self.assertTrue(self.supervisor_paths.runner.is_file())
            self.assertTrue(self.supervisor_paths.plist.is_file())

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

            exit_code, stdout, stderr = run_install_cli(
                "--status", "--state-path", str(result.state_path)
            )

            self.assertEqual(exit_code, 0)
            self.assertEqual(stderr, "")
            self.assertEqual(
                stdout,
                f"Codex Lab 1.2.3 from codex-lab-v1.2.3-lab.1\n"
                "Release version: 1.2.3-lab.1\n"
                "Bundle version: 42\n"
                "Source commit: abc123\n"
                f"App: {result.app_dir}\n"
                f"Shim: {result.shim_path}\n"
                "Code route: inactive\n"
                f"Engine: {result.engine_path}\n"
                f"Supervisor: {result.supervisor_label}\n"
                f"State: {result.state_path}\n",
            )

    def test_status_command_rejects_replaced_code_mode_host(self) -> None:
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
            assert result.code_mode_host_path is not None
            result.code_mode_host_path.write_text("replacement", encoding="utf-8")

            exit_code, _stdout, stderr = run_install_cli(
                "--status", "--state-path", str(result.state_path)
            )

            self.assertEqual(exit_code, 1)
            self.assertIn("Code Mode host identity", stderr)

    def test_status_command_reports_missing_install_state(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            state_path = Path(temp_dir) / "missing-state.json"
            resolved_state_path = (
                state_path.parent.resolve(strict=False) / state_path.name
            )

            for command in ["--status", "--check", "--update", "--uninstall"]:
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

    def test_old_shim_launches_force_rebuilt_app_without_regenerating_shim(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            v1_release = build_test_release(
                root / "v1",
                command="printf v1",
            )
            v2_release = build_test_release(
                root / "v2",
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
                bundle_version="43",
                commit="def456",
                command="printf v2",
            )
            app_dir = root / "install" / "Codex Lab.app"
            shim_dir = root / "install" / "bin"
            state_path = root / "install" / "install-state.json"
            install = install_from_manifest_url(
                v1_release.manifest_url,
                app_dir=app_dir,
                shim_dir=shim_dir,
                state_path=state_path,
                download=v1_release.download,
            )
            assert install.shim_path is not None
            old_shim = install.shim_path.read_text(encoding="utf-8")

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
                    download=v2_release.download,
                )

            assert update.install is not None
            self.assertEqual(update.install.version, "1.2.4")
            self.assertEqual(install.shim_path.read_text(encoding="utf-8"), old_shim)
            completed = subprocess.run(
                [str(install.shim_path)],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.stdout, "v2")

    def test_rejected_update_metadata_leaves_previous_install_usable(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            v1_release = build_test_release(
                root / "v1",
                command="printf v1",
            )
            bad_release = build_test_release(
                root / "bad",
                release_tag="codex-lab-v1.2.4-lab.1",
                version="1.2.4",
                bundle_version="43",
                commit="def456",
                command="printf bad",
            )
            bad_release.manifest["artifacts"]["appZip"]["sizeBytes"] += 1
            app_dir = root / "install" / "Codex Lab.app"
            shim_dir = root / "install" / "bin"
            state_path = root / "install" / "install-state.json"
            install = install_from_manifest_url(
                v1_release.manifest_url,
                app_dir=app_dir,
                shim_dir=shim_dir,
                state_path=state_path,
                download=v1_release.download,
            )
            assert install.shim_path is not None

            with (
                mock.patch(
                    "codex_lab_package.installer.lab_distribution_release_summaries",
                    return_value=[
                        lab_release_summary("codex-lab-v1.2.3-lab.1"),
                        lab_release_summary(
                            "codex-lab-v1.2.4-lab.1",
                            published_at="2026-06-08T02:00:00Z",
                        ),
                    ],
                ),
                self.assertRaisesRegex(ValueError, "sizeBytes does not match"),
            ):
                update_from_latest_release(
                    state_path=state_path,
                    download=bad_release.download,
                )

            status = read_install_state(state_path)
            self.assertEqual(status.version, "1.2.3")
            completed = subprocess.run(
                [str(install.shim_path)],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            )
            self.assertEqual(completed.stdout, "v1")

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
                "cbusillo/codex-lab",
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
                f"{sha256_file(release.dist_dir / SHIM_ZIP)}  {SHIM_ZIP}\n"
                f"{sha256_file(release.dist_dir / ENGINE_ZIP)}  {ENGINE_ZIP}\n",
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

    def test_preflights_state_parent_file_before_installing(self) -> None:
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

            with self.assertRaises(NotADirectoryError):
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
            self.assertFalse(self.supervisor_paths.managed_cli.exists())

    def test_post_install_smoke_failure_restores_prior_engine_and_files(self) -> None:
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
            self.supervisor_paths.managed_cli.parent.mkdir(parents=True)
            prior_engine = b"prior managed engine"
            self.supervisor_paths.managed_cli.write_bytes(prior_engine)
            os.chmod(self.supervisor_paths.managed_cli, 0o755)

            with (
                mock.patch(
                    "codex_lab_package.installer.smoke_check",
                    side_effect=[None, RuntimeError("installed smoke failed")],
                ),
                self.assertRaisesRegex(RuntimeError, "installed smoke failed"),
            ):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=app_dir,
                    shim_dir=shim_dir,
                    state_path=install_root / "state.json",
                    force=True,
                    download=release.download,
                )

            self.assertTrue((app_dir / "old-app-marker").is_file())
            self.assertEqual(
                (shim_dir / "codex-lab").read_text(encoding="utf-8"),
                "old shim",
            )
            self.assertEqual(
                self.supervisor_paths.managed_cli.read_bytes(),
                prior_engine,
            )
            self.assertFalse((install_root / "state.json").exists())
            self.assertEqual(self.supervisor_installs, [])

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


def fake_inspect_engine(path: Path) -> EngineIdentity:
    metadata = json.loads(path.read_text(encoding="utf-8"))
    return EngineIdentity(
        build_channel="release",
        build_profile="release",
        release_version=metadata["releaseVersion"],
        sha256=sha256_file(path),
        signing_identifier=ENGINE_SIGNING_IDENTIFIER,
        source_commit=metadata["sourceCommit"],
        team_identifier=ENGINE_TEAM_IDENTIFIER,
        version=metadata["version"],
    )


def fake_inspect_code_mode_host(path: Path) -> CodeModeHostIdentity:
    return CodeModeHostIdentity(
        sha256=sha256_file(path),
        signing_identifier=CODE_MODE_HOST_SIGNING_IDENTIFIER,
        team_identifier=ENGINE_TEAM_IDENTIFIER,
    )


def build_test_release(
    root: Path,
    *,
    release_tag: str = "codex-lab-v1.2.3-lab.1",
    version: str = "1.2.3",
    bundle_version: str = "42",
    commit: str = "abc123",
    command: str = 'exec /bin/sh "$@"',
) -> TestRelease:
    root.mkdir(parents=True, exist_ok=True)
    build_root = root / "build"
    dist_dir = root / "dist"
    dist_dir.mkdir()
    codex_bin = root / "fake-codex"
    write_file(codex_bin, f"#!/bin/sh\n{command}\n")
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
    engine_bin = root / "engine" / "codex"
    engine_bin.parent.mkdir()
    write_file(
        engine_bin,
        json.dumps(
            {
                "releaseVersion": release_tag.removeprefix("codex-lab-v"),
                "sourceCommit": commit,
                "version": version,
            }
        ),
    )
    os.chmod(engine_bin, 0o755)
    code_mode_host_bin = engine_bin.with_name("codex-code-mode-host")
    write_file(code_mode_host_bin, "signed Code Mode host fixture")
    os.chmod(code_mode_host_bin, 0o755)
    zip_tree(engine_bin.parent, dist_dir / ENGINE_ZIP, arcname=Path("engine"))
    write_file(
        dist_dir / "SHA256SUMS",
        f"{sha256_file(dist_dir / APP_ZIP)}  {APP_ZIP}\n"
        f"{sha256_file(dist_dir / SHIM_ZIP)}  {SHIM_ZIP}\n"
        f"{sha256_file(dist_dir / ENGINE_ZIP)}  {ENGINE_ZIP}\n",
    )

    manifest_url = manifest_url_for_release_tag(release_tag)
    manifest = build_manifest(
        dist_dir=dist_dir,
        checksums={
            APP_ZIP: sha256_file(dist_dir / APP_ZIP),
            SHIM_ZIP: sha256_file(dist_dir / SHIM_ZIP),
            ENGINE_ZIP: sha256_file(dist_dir / ENGINE_ZIP),
        },
        version=version,
        bundle_version=bundle_version,
        commit=commit,
        repository="cbusillo/codex-lab",
        workflow="codex-lab-release",
        run_id="100",
        run_attempt="1",
        release_tag=release_tag,
        download_base_url=manifest_url.rsplit("/", 1)[0],
        generated_at="2026-06-07T00:00:00Z",
        engine_signed=True,
    )
    return TestRelease(dist_dir, manifest_url, manifest)


def update_manifest_artifact(release: TestRelease, role: str, file_name: str) -> None:
    artifacts = {
        role: entry.copy() for role, entry in release.manifest["artifacts"].items()
    }
    release.manifest = {**release.manifest, "artifacts": artifacts}
    artifacts[role]["sha256"] = sha256_file(release.dist_dir / file_name)
    artifacts[role]["sizeBytes"] = (release.dist_dir / file_name).stat().st_size


def convert_test_release_to_schema_two(release: TestRelease) -> None:
    engine_zip = release.dist_dir / ENGINE_ZIP
    with ZipFile(engine_zip) as archive:
        engine_bytes = archive.read("engine/codex")
    engine_info = ZipInfo("codex")
    engine_info.external_attr = ((stat.S_IFREG | 0o755) & 0xFFFF) << 16
    with ZipFile(engine_zip, "w", compression=ZIP_DEFLATED) as archive:
        archive.writestr(engine_info, engine_bytes)
    release.manifest["schemaVersion"] = 2
    engine_artifact = release.manifest["artifacts"]["engineZip"]
    engine_artifact["archiveRoot"] = "codex"
    engine_artifact["sha256"] = sha256_file(engine_zip)
    engine_artifact["sizeBytes"] = engine_zip.stat().st_size
    managed_engine = release.manifest["managedEngine"]
    managed_engine.pop("companions")
    managed_engine["requiredEntitlements"] = ["com.apple.security.cs.allow-jit"]
    managed_engine["sha256"] = sha256_zip_member(engine_zip, "codex")
    write_file(
        release.dist_dir / "SHA256SUMS",
        "".join(
            f"{sha256_file(release.dist_dir / file_name)}  {file_name}\n"
            for file_name in (APP_ZIP, SHIM_ZIP, ENGINE_ZIP)
        ),
    )


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
