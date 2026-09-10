import hashlib
import json
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import textwrap
import unittest

from voice_sdk_ci import IDENTITY_PATHS
from voice_sdk_ci import identity
from voice_sdk_ci import verify


class VoiceSdkCiTest(unittest.TestCase):
    def test_native_provisioning_changes_admit_workspace_compile(self):
        repository = Path(__file__).resolve().parents[2]
        workflow = (repository / ".github/workflows/rust-ci.yml").read_text()
        selection = re.search(
            r"(?ms)^          codex=false\n.*?^          done$", workflow
        ).group()
        command = (
            'files=("$@")\n' + textwrap.dedent(selection) + '\nprintf "%s" "$codex"'
        )
        for path, expected in (
            (".github/actions/setup-voice-sdk/action.yml", "true"),
            (".github/scripts/voice_sdk_ci.py", "true"),
            (".github/scripts/test_voice_sdk_ci.py", "true"),
            ("patches/llvm_reject_sdk_finder_metadata.patch", "true"),
            ("third_party/voice/build_native.py", "true"),
            ("README.md", "false"),
        ):
            with self.subTest(path=path):
                self.assertEqual(
                    subprocess.check_output(
                        ["bash", "-c", command, "selection", path], text=True
                    ),
                    expected,
                )

    def test_identity_changes_with_native_input_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative in IDENTITY_PATHS:
                path = root / relative
                if relative in ("third_party/voice", "patches"):
                    path.mkdir(parents=True)
                    path /= (
                        "sources.json"
                        if relative == "third_party/voice"
                        else "llvm.patch"
                    )
                else:
                    path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(relative)
            subprocess.run(["git", "init", "-q", root], check=True)
            subprocess.run(["git", "-C", root, "add", "."], check=True)
            before = identity(root, "aarch64-apple-darwin", "toolchain")
            (root / "third_party/voice/sources.json").write_text("changed")
            self.assertNotEqual(
                before, identity(root, "aarch64-apple-darwin", "toolchain")
            )
            after_source = identity(root, "aarch64-apple-darwin", "toolchain")
            (root / "patches/llvm.patch").write_text("new compiler behavior")
            self.assertNotEqual(
                after_source, identity(root, "aarch64-apple-darwin", "toolchain")
            )

    @staticmethod
    def fixture(root: Path) -> tuple[Path, Path, Path]:
        sdk = root / "sdk with spaces"
        sources = root / "sources.json"
        sources.write_text("{}")
        pc = sdk / "lib/pkgconfig/gstreamer-1.0.pc"
        pc.parent.mkdir(parents=True)
        pc.write_text("prefix=${pcfiledir}/../..\n")
        digest = hashlib.sha256(pc.read_bytes()).hexdigest()
        (sdk / "sdk.json").write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "target": "aarch64-apple-darwin",
                    "sourceCommit": "0" * 40,
                    "sourceManifestSha256": hashlib.sha256(
                        sources.read_bytes()
                    ).hexdigest(),
                    "files": [
                        {"path": "lib/pkgconfig/gstreamer-1.0.pc", "sha256": digest}
                    ],
                }
            )
        )
        tool = root / "pkg-config"
        tool.write_text(
            '#!/bin/sh\ncase "$1" in --atleast-version=1.28) exit 0;; esac\ncase "$*" in *gstreamer-base-1.0*gstreamer-app-1.0*gstreamer-audio-1.0*) ;; *) exit 9;; esac\necho "-I\'$PKG_CONFIG_LIBDIR/../../include\' -L\'$PKG_CONFIG_LIBDIR/..\'"\n'
        )
        tool.chmod(tool.stat().st_mode | stat.S_IXUSR)
        return sdk, tool, sources

    def test_verified_inventory_and_contained_pkg_config_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            sdk, tool, sources = self.fixture(Path(directory))
            verify(sdk, "aarch64-apple-darwin", tool, sources)

    def test_digest_mismatch_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            sdk, tool, sources = self.fixture(Path(directory))
            (sdk / "lib/pkgconfig/gstreamer-1.0.pc").write_text("changed")
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                verify(sdk, "aarch64-apple-darwin", tool, sources)

    def test_host_path_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            sdk, tool, sources = self.fixture(Path(directory))
            tool.write_text(
                "#!/bin/sh\ncase \"$1\" in --atleast-version=1.28) exit 0;; esac\necho '-I/opt/homebrew/include'\n"
            )
            with self.assertRaisesRegex(ValueError, "escaped"):
                verify(sdk, "aarch64-apple-darwin", tool, sources)

    def test_extra_file_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            sdk, tool, sources = self.fixture(Path(directory))
            (sdk / "lib/pkgconfig/extra.pc").write_text("host fallback")
            with self.assertRaisesRegex(ValueError, "unverified file"):
                verify(sdk, "aarch64-apple-darwin", tool, sources)

    def test_linked_or_oversized_receipt_fails_before_reading(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sdk, tool, sources = self.fixture(root)
            receipt = sdk / "sdk.json"
            outside = root / "receipt.json"
            receipt.rename(outside)
            receipt.symlink_to(outside)
            with self.assertRaisesRegex(ValueError, "bounded regular file"):
                verify(sdk, "aarch64-apple-darwin", tool, sources)
            receipt.unlink()
            with receipt.open("wb") as output:
                output.truncate(2 * 1024 * 1024 + 1)
            with self.assertRaisesRegex(ValueError, "bounded regular file"):
                verify(sdk, "aarch64-apple-darwin", tool, sources)

    def test_symlinked_directory_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sdk, tool, sources = self.fixture(root)
            (sdk / "linked").symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlinked directory"):
                verify(sdk, "aarch64-apple-darwin", tool, sources)

    def test_target_and_manifest_mismatch_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            sdk, tool, sources = self.fixture(Path(directory))
            with self.assertRaisesRegex(ValueError, "metadata"):
                verify(sdk, "x86_64-apple-darwin", tool, sources)
            sources.write_text("changed")
            with self.assertRaisesRegex(ValueError, "metadata"):
                verify(sdk, "aarch64-apple-darwin", tool, sources)


if __name__ == "__main__":
    unittest.main()
