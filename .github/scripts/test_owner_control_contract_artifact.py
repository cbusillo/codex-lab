#!/usr/bin/env python3

"""Pin the exact Launchplane owner-control v5 artifact and provenance."""

import hashlib
import json
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ARTIFACT = ROOT / "codex-rs/owner-control-contract/contracts/owner-control-contract.json"
RUST_SOURCE = ROOT / "codex-rs/owner-control-contract/src/artifact.rs"
README = ROOT / "codex-rs/owner-control-contract/README.md"
EXPECTED_SCHEMA_VERSION = 5
EXPECTED_SHA256 = "cf2815b65bafb7e25b00647dbdfd464577cb0a6e8a861ae3e1e019840865804e"
UPSTREAM_REVIEWED_HEAD = "bb20d9ae6754c7c408ea275e9a135d39f2cb971d"
UPSTREAM_MERGE_COMMIT = "6e60897eebd6ee2ba2a3bc234e85de531c8298a0"


class OwnerControlContractArtifactTest(unittest.TestCase):
    def test_exact_v5_artifact_and_upstream_provenance_are_pinned(self) -> None:
        artifact_bytes = ARTIFACT.read_bytes()
        artifact = json.loads(artifact_bytes)
        rust_source = RUST_SOURCE.read_text()
        readme = README.read_text()

        self.assertEqual(artifact["schema_version"], EXPECTED_SCHEMA_VERSION)
        self.assertEqual(hashlib.sha256(artifact_bytes).hexdigest(), EXPECTED_SHA256)
        self.assertIn(f'"{EXPECTED_SHA256}"', rust_source)
        self.assertIn(
            f"OWNER_CONTROL_CONTRACT_SCHEMA_VERSION: u8 = {EXPECTED_SCHEMA_VERSION}",
            rust_source,
        )
        for expected in (
            EXPECTED_SHA256,
            UPSTREAM_REVIEWED_HEAD,
            UPSTREAM_MERGE_COMMIT,
        ):
            self.assertIn(expected, readme)


if __name__ == "__main__":
    unittest.main()
