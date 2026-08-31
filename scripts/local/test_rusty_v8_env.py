import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest import mock


MODULE_PATH = Path(__file__).with_name("rusty_v8_env.py")
SPEC = importlib.util.spec_from_file_location("rusty_v8_env", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"failed to load {MODULE_PATH}")
rusty_v8_env: Any = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(rusty_v8_env)


def patch_rusty_v8(name: str, **kwargs: Any) -> Any:
    return mock.patch.object(rusty_v8_env, name, **kwargs)


class RustyV8EnvTest(unittest.TestCase):
    def test_cache_root_prefers_developer_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            environment = {"CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT": temporary_directory}

            root = rusty_v8_env.cache_root(environment)

        self.assertEqual(
            root,
            Path(temporary_directory) / "local" / "codex-lab" / "rusty-v8",
        )

    def test_status_cache_kind_preserves_custom_override_precedence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            with (
                patch_rusty_v8("resolved_version", return_value="1.2.3"),
                patch_rusty_v8("host_target", return_value="aarch64-apple-darwin"),
                patch_rusty_v8("manifest_checksums", return_value={}),
            ):
                result = rusty_v8_env.status(
                    {
                        "CODEX_LAB_RUSTY_V8_CACHE_DIR": f"{temporary_directory}/custom",
                        "CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT": temporary_directory,
                    }
                )

        self.assertEqual(result["cacheKind"], "custom")

    def test_verify_cached_rejects_tampering_and_uses_stamp(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            path = Path(temporary_directory) / "artifact"
            path.write_bytes(b"trusted")
            expected = hashlib.sha256(b"trusted").hexdigest()

            self.assertTrue(rusty_v8_env.verify_cached(path, expected))
            stamp = json.loads(
                path.with_name("artifact.verified.json").read_text(encoding="utf-8")
            )
            with patch_rusty_v8("sha256_file", side_effect=AssertionError("rehash")):
                self.assertTrue(rusty_v8_env.verify_cached(path, expected))
            path.write_bytes(b"tampered")
            self.assertFalse(rusty_v8_env.verify_cached(path, expected))
            self.assertFalse(path.exists())
            self.assertEqual(stamp["sha256"], expected)

    def test_asset_names_match_manifest_convention(self) -> None:
        self.assertEqual(
            rusty_v8_env.asset_names("aarch64-apple-darwin"),
            (
                "librusty_v8_ptrcomp_sandbox_release_aarch64-apple-darwin.a.gz",
                "src_binding_ptrcomp_sandbox_release_aarch64-apple-darwin.rs",
            ),
        )

    def test_network_failure_is_fail_open_unless_required(self) -> None:
        with patch_rusty_v8("resolve", side_effect=RuntimeError("truncated download")):
            self.assertEqual(rusty_v8_env.main(["resolve"]), 0)
            self.assertEqual(rusty_v8_env.main(["resolve", "--require"]), 2)


if __name__ == "__main__":
    unittest.main()
