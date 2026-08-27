import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("upstream_candidate_preflight.py")
SPEC = importlib.util.spec_from_file_location("upstream_candidate_preflight", SCRIPT)
preflight = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(preflight)


class CandidatePreflightTest(unittest.TestCase):
    def test_helper_has_no_candidate_execution_primitives(self) -> None:
        contents = SCRIPT.read_text(encoding="utf-8")

        for forbidden in ("subprocess", "os.system", "build.rs", "exec("):
            self.assertNotIn(forbidden, contents)

    def candidate_tree(self, root: Path) -> Path:
        candidate = root / "candidate"
        lock = candidate / "codex-rs" / "Cargo.lock"
        manifest = candidate / "third_party" / "v8" / "rusty_v8_1_2_3_codex_release.sha256"
        lock.parent.mkdir(parents=True)
        manifest.parent.mkdir(parents=True)
        lock.write_text(
            """[[package]]
name = \"v8\"
version = \"1.2.3\"
source = \"registry+https://github.com/rust-lang/crates.io-index\"
checksum = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"
""",
            encoding="utf-8",
        )
        manifest.write_text(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  librusty_v8_ptrcomp_sandbox_release_aarch64-apple-darwin.a.gz\n"
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  src_binding_ptrcomp_sandbox_release_aarch64-apple-darwin.rs\n",
            encoding="utf-8",
        )
        return candidate

    def test_reads_v8_lock_and_release_manifest_as_data(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            candidate = self.candidate_tree(Path(temporary_directory))

            version, crate_checksum = preflight.candidate_v8_package(candidate)
            checksums = preflight.release_checksums(candidate, version)

        self.assertEqual(version, "1.2.3")
        self.assertEqual(
            crate_checksum,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        self.assertEqual(
            checksums["src_binding_ptrcomp_sandbox_release_aarch64-apple-darwin.rs"],
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        )

    def test_preflight_verifies_downloads_without_executing_candidate_code(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            candidate = self.candidate_tree(root)
            payloads = {
                "librusty_v8_ptrcomp_sandbox_release_aarch64-apple-darwin.a.gz": b"archive",
                "src_binding_ptrcomp_sandbox_release_aarch64-apple-darwin.rs": b"binding",
            }
            manifest = candidate / "third_party/v8/rusty_v8_1_2_3_codex_release.sha256"
            manifest.write_text(
                "\n".join(
                    f"{hashlib.sha256(payload).hexdigest()}  {name}"
                    for name, payload in payloads.items()
                )
                + "\n",
                encoding="utf-8",
            )

            def download(url: str, expected_checksum: str, destination: Path) -> int:
                payload = payloads[url.rsplit('/', 1)[1]]
                destination.write_bytes(payload)
                self.assertEqual(hashlib.sha256(payload).hexdigest(), expected_checksum)
                return len(payload)

            with patch.object(preflight, "verified_download", side_effect=download):
                status = preflight.run_preflight(
                    candidate,
                    root / "downloads",
                    root / "preflight.json",
                )

            result = json.loads((root / "preflight.json").read_text(encoding="utf-8"))
        self.assertEqual(status, 0)
        self.assertEqual(result["status"], "passed")
        self.assertEqual([asset["bytes"] for asset in result["assets"]], [7, 7])

    def test_evidence_caps_conflict_paths_and_requires_exact_refs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            conflicts = root / "conflicts.txt"
            conflicts.write_text(
                "\n".join(f"path-{number}" for number in range(250)) + "\n",
                encoding="utf-8",
            )
            args = type(
                "Args",
                (),
                {
                    "classification": "conflict",
                    "reason": "conflict",
                    "base": "a" * 40,
                    "upstream": "b" * 40,
                    "local": "c" * 40,
                    "gate_status": "ready",
                    "snapshot": "upstream/openai-codex/example",
                    "conflicts": conflicts,
                    "conflict_total": 250,
                    "preflight": root / "missing.json",
                    "worktree_removed": "true",
                    "primary_checkout_clean": "true",
                    "output_dir": root / "evidence",
                    "workflow_sha": "d" * 40,
                    "run_id": "12345",
                },
            )()

            preflight.write_evidence(args)
            result = json.loads((root / "evidence/candidate-evidence.json").read_text())

        self.assertEqual(result["conflictPathTotal"], 250)
        self.assertTrue(result["conflictPathsTruncated"])
        self.assertTrue(result["temporaryWorktreeRemoved"])
        self.assertTrue(result["primaryCheckoutClean"])
