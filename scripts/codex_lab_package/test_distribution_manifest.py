#!/usr/bin/env python3

from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.distribution_manifest import APP_ZIP
from codex_lab_package.distribution_manifest import SHIM_ZIP
from codex_lab_package.distribution_manifest import build_manifest
from codex_lab_package.distribution_manifest import read_sha256sums
from codex_lab_package.distribution_manifest import sha256_file
from codex_lab_package.distribution_manifest import validate_manifest


class DistributionManifestTest(unittest.TestCase):
    def test_builds_and_validates_distribution_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dist_dir = Path(temp_dir)
            app_zip = dist_dir / APP_ZIP
            shim_zip = dist_dir / SHIM_ZIP
            _copy_fixture(app_zip)
            _copy_fixture(shim_zip)
            sha256sums = dist_dir / "SHA256SUMS"
            _write_sha256sums(sha256sums, app_zip, shim_zip)

            manifest = build_manifest(
                dist_dir=dist_dir,
                checksums=read_sha256sums(sha256sums),
                version="1.2.3",
                bundle_version="42",
                commit="abc123",
                repository="cbusillo/codex-lab",
                workflow="codex-lab-app",
                run_id="100",
                run_attempt="2",
                release_tag="codex-lab-v1.2.3-lab.42",
                download_base_url="https://github.com/cbusillo/codex-lab/releases/download/codex-lab-v1.2.3-lab.42/",
                generated_at="2026-06-07T00:00:00Z",
            )

            self.assertEqual(manifest["version"], "1.2.3")
            self.assertEqual(manifest["bundleVersion"], "42")
            self.assertEqual(manifest["product"], "codex-lab")
            self.assertEqual(manifest["channel"], "lab")
            self.assertEqual(manifest["platform"], "aarch64-apple-darwin")
            self.assertEqual(manifest["schemaVersion"], 1)
            self.assertEqual(manifest["source"]["commit"], "abc123")
            self.assertEqual(manifest["release"], {"tag": "codex-lab-v1.2.3-lab.42"})
            self.assertEqual(
                {layout["kind"] for layout in manifest["supportedLayouts"]},
                {"applicationsFolder", "envOverride", "extractedSibling"},
            )
            self.assertEqual(
                manifest["desktopIntegration"],
                {
                    "cliOverrideEnv": "CODEX_CLI_PATH",
                    "launchesFreshInstance": True,
                    "officialCodexAppPath": "/Applications/Codex.app",
                    "requiresOfficialCodexApp": True,
                },
            )
            self.assertEqual(
                manifest["artifacts"]["appZip"],
                {
                    "archiveRoot": "Codex Lab.app",
                    "description": "Canonical app update unit containing the embedded Codex Lab CLI.",
                    "fileName": APP_ZIP,
                    "downloadUrl": "https://github.com/cbusillo/codex-lab/releases/download/codex-lab-v1.2.3-lab.42/"
                    + APP_ZIP,
                    "notarized": False,
                    "sha256": sha256_file(app_zip),
                    "signed": False,
                    "sizeBytes": app_zip.stat().st_size,
                },
            )
            self.assertEqual(
                manifest["artifacts"]["shimZip"],
                {
                    "archiveRoot": "bin/codex-lab",
                    "description": "Companion CLI wrapper that resolves an installed or sibling Codex Lab.app.",
                    "fileName": SHIM_ZIP,
                    "downloadUrl": "https://github.com/cbusillo/codex-lab/releases/download/codex-lab-v1.2.3-lab.42/"
                    + SHIM_ZIP,
                    "notarized": False,
                    "sha256": sha256_file(shim_zip),
                    "signed": False,
                    "sizeBytes": shim_zip.stat().st_size,
                },
            )
            validate_manifest(
                manifest,
                dist_dir=dist_dir,
                checksums=read_sha256sums(sha256sums),
            )

    def test_rejects_checksum_manifest_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            sha256sums = Path(temp_dir) / "SHA256SUMS"
            subprocess.run(
                [
                    "/bin/sh",
                    "-c",
                    'printf \'%s  %s\\n\' "$1" "$2" > "$3"',
                    "sh",
                    "0" * 64,
                    APP_ZIP,
                    str(sha256sums),
                ],
                check=True,
            )

            with self.assertRaisesRegex(ValueError, "artifact mismatch"):
                read_sha256sums(sha256sums)

    def test_rejects_signed_artifacts_until_signing_is_real(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dist_dir = Path(temp_dir)
            app_zip = dist_dir / APP_ZIP
            shim_zip = dist_dir / SHIM_ZIP
            _copy_fixture(app_zip)
            _copy_fixture(shim_zip)
            checksums = {APP_ZIP: sha256_file(app_zip), SHIM_ZIP: sha256_file(shim_zip)}
            manifest = build_manifest(
                dist_dir=dist_dir,
                checksums=checksums,
                version="1.2.3",
                bundle_version="42",
                commit="abc123",
                repository="cbusillo/codex-lab",
                workflow="codex-lab-app",
                run_id="100",
                run_attempt="2",
                generated_at="2026-06-07T00:00:00Z",
            )
            manifest["artifacts"]["appZip"]["signed"] = True

            with self.assertRaisesRegex(ValueError, "unsigned and not notarized"):
                validate_manifest(manifest)

    def test_rejects_invalid_download_url(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dist_dir = Path(temp_dir)
            app_zip = dist_dir / APP_ZIP
            shim_zip = dist_dir / SHIM_ZIP
            _copy_fixture(app_zip)
            _copy_fixture(shim_zip)
            checksums = {APP_ZIP: sha256_file(app_zip), SHIM_ZIP: sha256_file(shim_zip)}
            manifest = build_manifest(
                dist_dir=dist_dir,
                checksums=checksums,
                version="1.2.3",
                bundle_version="42",
                commit="abc123",
                repository="cbusillo/codex-lab",
                workflow="codex-lab-app",
                run_id="100",
                run_attempt="2",
                generated_at="2026-06-07T00:00:00Z",
            )
            manifest["artifacts"]["appZip"]["downloadUrl"] = (
                "http://example.invalid/app.zip"
            )

            with self.assertRaisesRegex(ValueError, "invalid downloadUrl"):
                validate_manifest(manifest)

    def test_rejects_download_url_for_wrong_release(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dist_dir = Path(temp_dir)
            app_zip = dist_dir / APP_ZIP
            shim_zip = dist_dir / SHIM_ZIP
            _copy_fixture(app_zip)
            _copy_fixture(shim_zip)
            checksums = {APP_ZIP: sha256_file(app_zip), SHIM_ZIP: sha256_file(shim_zip)}
            manifest = build_manifest(
                dist_dir=dist_dir,
                checksums=checksums,
                version="1.2.3",
                bundle_version="42",
                commit="abc123",
                repository="cbusillo/codex-lab",
                workflow="codex-lab-app",
                run_id="100",
                run_attempt="2",
                release_tag="codex-lab-v1.2.3",
                download_base_url="https://github.com/cbusillo/codex-lab/releases/download/codex-lab-v1.2.3",
                generated_at="2026-06-07T00:00:00Z",
            )
            manifest["artifacts"]["appZip"]["downloadUrl"] = (
                "https://github.com/cbusillo/codex-lab/releases/download/codex-lab-v9.9.9/"
                + APP_ZIP
            )

            with self.assertRaisesRegex(ValueError, "must target release artifact"):
                validate_manifest(manifest)

    def test_rejects_release_without_download_urls(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dist_dir = Path(temp_dir)
            app_zip = dist_dir / APP_ZIP
            shim_zip = dist_dir / SHIM_ZIP
            _copy_fixture(app_zip)
            _copy_fixture(shim_zip)
            checksums = {APP_ZIP: sha256_file(app_zip), SHIM_ZIP: sha256_file(shim_zip)}
            manifest = build_manifest(
                dist_dir=dist_dir,
                checksums=checksums,
                version="1.2.3",
                bundle_version="42",
                commit="abc123",
                repository="cbusillo/codex-lab",
                workflow="codex-lab-app",
                run_id="100",
                run_attempt="2",
                generated_at="2026-06-07T00:00:00Z",
            )
            manifest["release"] = {"tag": "codex-lab-v1.2.3"}

            with self.assertRaisesRegex(ValueError, "provided together"):
                validate_manifest(manifest)

    def test_rejects_partial_release_download_urls(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dist_dir = Path(temp_dir)
            app_zip = dist_dir / APP_ZIP
            shim_zip = dist_dir / SHIM_ZIP
            _copy_fixture(app_zip)
            _copy_fixture(shim_zip)
            checksums = {APP_ZIP: sha256_file(app_zip), SHIM_ZIP: sha256_file(shim_zip)}
            manifest = build_manifest(
                dist_dir=dist_dir,
                checksums=checksums,
                version="1.2.3",
                bundle_version="42",
                commit="abc123",
                repository="cbusillo/codex-lab",
                workflow="codex-lab-app",
                run_id="100",
                run_attempt="2",
                release_tag="codex-lab-v1.2.3",
                download_base_url="https://example.invalid/downloads",
                generated_at="2026-06-07T00:00:00Z",
            )
            del manifest["artifacts"]["shimZip"]["downloadUrl"]

            with self.assertRaisesRegex(ValueError, "every artifact"):
                validate_manifest(manifest)

    def test_requires_release_tag_with_download_base_url(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dist_dir = Path(temp_dir)
            app_zip = dist_dir / APP_ZIP
            shim_zip = dist_dir / SHIM_ZIP
            _copy_fixture(app_zip)
            _copy_fixture(shim_zip)
            checksums = {APP_ZIP: sha256_file(app_zip), SHIM_ZIP: sha256_file(shim_zip)}

            with self.assertRaisesRegex(ValueError, "provided together"):
                build_manifest(
                    dist_dir=dist_dir,
                    checksums=checksums,
                    version="1.2.3",
                    bundle_version="42",
                    commit="abc123",
                    repository="cbusillo/codex-lab",
                    workflow="codex-lab-app",
                    run_id="100",
                    run_attempt="2",
                    download_base_url="https://example.invalid/downloads",
                    generated_at="2026-06-07T00:00:00Z",
                )

            with self.assertRaisesRegex(ValueError, "provided together"):
                build_manifest(
                    dist_dir=dist_dir,
                    checksums=checksums,
                    version="1.2.3",
                    bundle_version="42",
                    commit="abc123",
                    repository="cbusillo/codex-lab",
                    workflow="codex-lab-app",
                    run_id="100",
                    run_attempt="2",
                    release_tag="codex-lab-v1.2.3",
                    generated_at="2026-06-07T00:00:00Z",
                )

    def test_rejects_malformed_release_tag(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dist_dir = Path(temp_dir)
            app_zip = dist_dir / APP_ZIP
            shim_zip = dist_dir / SHIM_ZIP
            _copy_fixture(app_zip)
            _copy_fixture(shim_zip)
            checksums = {APP_ZIP: sha256_file(app_zip), SHIM_ZIP: sha256_file(shim_zip)}
            manifest = build_manifest(
                dist_dir=dist_dir,
                checksums=checksums,
                version="1.2.3",
                bundle_version="42",
                commit="abc123",
                repository="cbusillo/codex-lab",
                workflow="codex-lab-app",
                run_id="100",
                run_attempt="2",
                release_tag="codex-lab-v1.2.3",
                download_base_url="https://example.invalid/downloads",
                generated_at="2026-06-07T00:00:00Z",
            )
            manifest["release"]["tag"] = "not a codex lab release tag"

            with self.assertRaisesRegex(ValueError, "invalid format"):
                validate_manifest(manifest)

    def test_rejects_release_tag_version_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            dist_dir = Path(temp_dir)
            app_zip = dist_dir / APP_ZIP
            shim_zip = dist_dir / SHIM_ZIP
            _copy_fixture(app_zip)
            _copy_fixture(shim_zip)
            checksums = {APP_ZIP: sha256_file(app_zip), SHIM_ZIP: sha256_file(shim_zip)}
            manifest = build_manifest(
                dist_dir=dist_dir,
                checksums=checksums,
                version="1.2.3",
                bundle_version="42",
                commit="abc123",
                repository="cbusillo/codex-lab",
                workflow="codex-lab-app",
                run_id="100",
                run_attempt="2",
                release_tag="codex-lab-v9.9.9",
                download_base_url="https://example.invalid/downloads",
                generated_at="2026-06-07T00:00:00Z",
            )

            with self.assertRaisesRegex(ValueError, "does not match"):
                validate_manifest(manifest)


def _copy_fixture(dest: Path) -> None:
    shutil.copyfile(__file__, dest)


def _write_sha256sums(path: Path, app_zip: Path, shim_zip: Path) -> None:
    subprocess.run(
        [
            "/bin/sh",
            "-c",
            'printf \'%s  %s\\n%s  %s\\n\' "$1" "$2" "$3" "$4" > "$5"',
            "sh",
            sha256_file(app_zip),
            APP_ZIP,
            sha256_file(shim_zip),
            SHIM_ZIP,
            str(path),
        ],
        check=True,
    )


if __name__ == "__main__":
    unittest.main()
