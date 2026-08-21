import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SIGNING_WRAPPER = ROOT / ".github/scripts/macos-signing/sign_macos_code.sh"


class MacosSigningWrapperTest(unittest.TestCase):
    def test_native_codesign_is_scoped_to_the_selected_keychain(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            arguments_file = root / "codesign-arguments"
            fake_codesign = bin_dir / "codesign"
            fake_codesign.write_text(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CODESIGN_ARGUMENTS_FILE\"\n",
                encoding="utf-8",
            )
            fake_codesign.chmod(0o755)
            target = root / "codex"
            target.write_text("binary", encoding="utf-8")
            keychain = root / "signing.keychain-db"

            result = subprocess.run(
                [
                    "bash",
                    str(SIGNING_WRAPPER),
                    "--target",
                    str(target),
                    "--identity",
                    "Developer ID Application: Example",
                    "--keychain",
                    str(keychain),
                    "--timestamp",
                    "false",
                ],
                check=False,
                capture_output=True,
                env={
                    **os.environ,
                    "CODESIGN_ARGUMENTS_FILE": str(arguments_file),
                    "PATH": f"{bin_dir}:{os.environ['PATH']}",
                },
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            arguments = arguments_file.read_text(encoding="utf-8").splitlines()
            self.assertEqual(
                arguments[arguments.index("--keychain") + 1],
                str(keychain),
            )

    def test_rcodesign_rejects_a_native_keychain_argument(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "codex"
            target.write_text("binary", encoding="utf-8")

            result = subprocess.run(
                [
                    "bash",
                    str(SIGNING_WRAPPER),
                    "--target",
                    str(target),
                    "--identity",
                    "Developer ID Application: Example",
                    "--keychain",
                    str(root / "signing.keychain-db"),
                ],
                check=False,
                capture_output=True,
                env={**os.environ, "OAI_CODESIGN_BACKEND": "akv-pkcs11"},
                text=True,
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn("supported only by the native codesign backend", result.stderr)


if __name__ == "__main__":
    unittest.main()
