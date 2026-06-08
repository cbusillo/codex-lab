#!/usr/bin/env python3

from pathlib import Path
from zipfile import ZIP_DEFLATED
from zipfile import ZipFile
from zipfile import ZipInfo
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
from codex_lab_package.installer import download_url
from codex_lab_package.installer import install_from_manifest_url
from codex_lab_package.installer import manifest_url_for_release_tag
from codex_lab_package.installer import replace_path
from codex_lab_package.layout import CodexLabAppOptions
from codex_lab_package.layout import build_codex_lab_app


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
                result.app_dir, install_root.absolute() / "Custom Codex Lab.app"
            )
            self.assertEqual(
                result.shim_path, install_root.absolute() / "bin" / "codex-lab"
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

    def test_rejects_symlink_app_parent_before_resolving_it(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            real_apps = root / "real-apps"
            real_apps.mkdir()
            app_parent = root / "install" / "Apps"
            app_parent.parent.mkdir()
            app_parent.symlink_to(real_apps)

            with self.assertRaisesRegex(ValueError, "parent must not be a symlink"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=app_parent / "Codex Lab.app",
                    shim_dir=root / "install" / "bin",
                    state_path=root / "install" / "state.json",
                    force=True,
                    download=release.download,
                )

            self.assertTrue(app_parent.is_symlink())
            self.assertFalse((real_apps / "Codex Lab.app").exists())

    def test_rejects_symlink_shim_parent_before_resolving_it(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            release = build_test_release(root)
            real_bin = root / "real-bin"
            real_bin.mkdir()
            shim_dir = root / "install" / "bin"
            shim_dir.parent.mkdir()
            shim_dir.symlink_to(real_bin)

            with self.assertRaisesRegex(ValueError, "parent must not be a symlink"):
                install_from_manifest_url(
                    release.manifest_url,
                    app_dir=root / "install" / "Codex Lab.app",
                    shim_dir=shim_dir,
                    state_path=root / "install" / "state.json",
                    force=True,
                    download=release.download,
                )

            self.assertTrue(shim_dir.is_symlink())
            self.assertFalse((real_bin / "codex-lab").exists())

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


def build_test_release(root: Path) -> TestRelease:
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
            short_version="1.2.3",
            bundle_version="42",
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

    release_tag = "codex-lab-v1.2.3-lab.1"
    manifest_url = manifest_url_for_release_tag(release_tag)
    manifest = build_manifest(
        dist_dir=dist_dir,
        checksums={
            APP_ZIP: sha256_file(dist_dir / APP_ZIP),
            SHIM_ZIP: sha256_file(dist_dir / SHIM_ZIP),
        },
        version="1.2.3",
        bundle_version="42",
        commit="abc123",
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
