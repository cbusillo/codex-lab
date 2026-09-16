import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import upstream_convergence_gates as gates


def write_fixture(
    root: Path,
    *,
    contract_id: str = "HOME-1",
    evidence: list[dict[str, object]] | None = None,
) -> tuple[Path, Path]:
    if evidence is None:
        evidence = [
            {
                "kind": "symbol",
                "path": "proof.rs",
                "token": "fn proof()",
                "ciTier": "nightly",
            }
        ]
    manifest = root / "gates.json"
    manifest.write_text(
        json.dumps(
            {
                "schemaVersion": 1,
                "contracts": [{"id": contract_id, "evidence": evidence}],
            }
        ),
        encoding="utf-8",
    )
    contracts = root / "contracts.md"
    contracts.write_text(
        "| Contract | Surface | Lane | Required behavior | Current gate |\n"
        "| --- | --- | --- | --- | --- |\n"
        f"| `{contract_id}` | state | owned | behavior | proof |\n",
        encoding="utf-8",
    )
    return manifest, contracts


def semantic_reachability_fixture(
    edges: list[dict[str, object]],
    *,
    waivers: list[dict[str, object]] | None = None,
    max_waivers: int = 0,
) -> dict[str, object]:
    return {
        "kind": "semantic_reachability",
        "description": "production edge proof",
        "baseline": {
            "derivedFrom": "baseline.json",
            "issue": 664,
            "maxWaivers": max_waivers,
        },
        "edges": edges,
        "waivers": waivers or [],
        "ciTier": "blocking",
    }


class GateManifestTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        (self.root / "proof.rs").write_text("fn proof() {}\n", encoding="utf-8")
        (self.root / "baseline.json").write_text(
            json.dumps(
                {"schemaVersion": 1, "contracts": {"HOME-1": {"maxWaivers": 1}}}
            ),
            encoding="utf-8",
        )

    def verify_fixture(self, manifest: Path, contracts: Path) -> dict[str, object]:
        return gates.verify(self.root, manifest, contracts)

    def test_accepts_file_backed_and_narrative_evidence(self) -> None:
        manifest, contracts = write_fixture(
            self.root,
            evidence=[
                {
                    "kind": "symbol",
                    "path": "proof.rs",
                    "token": "fn proof()",
                    "ciTier": "nightly",
                },
                {
                    "kind": "narrative",
                    "description": "Requires an external service.",
                    "issue": 1,
                    "ciTier": "release",
                },
            ],
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertEqual(
            {
                "schemaVersion": 1,
                "contracts": 1,
                "evidence": 2,
                "tierCounts": {"nightly": 1, "release": 1},
                "errors": [],
                "passed": True,
            },
            report,
        )

    def test_reports_unreachable_rust_module_edge(self) -> None:
        (self.root / "lib.rs").write_text("", encoding="utf-8")
        (self.root / "feature.rs").write_text("fn install() {}\n", encoding="utf-8")
        manifest, contracts = write_fixture(
            self.root,
            evidence=[
                semantic_reachability_fixture(
                    [
                        {
                            "id": "feature-module",
                            "kind": "rust_module",
                            "root": "lib.rs",
                            "path": "feature.rs",
                            "token": "",
                        }
                    ]
                )
            ],
        )
        report = self.verify_fixture(manifest, contracts)
        self.assertFalse(report["passed"])
        self.assertTrue(
            any(
                "feature.rs is not reachable from Rust module root lib.rs" in error
                for error in report["errors"]
            )
        )

    def test_reports_token_edge_in_unreachable_rust_module(self) -> None:
        (self.root / "lib.rs").write_text("", encoding="utf-8")
        (self.root / "feature.rs").write_text("fn install() {}\n", encoding="utf-8")
        manifest, contracts = write_fixture(
            self.root,
            evidence=[
                semantic_reachability_fixture(
                    [
                        {
                            "id": "feature-callsite",
                            "kind": "token",
                            "root": "lib.rs",
                            "path": "feature.rs",
                            "token": "install()",
                        }
                    ]
                )
            ],
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertFalse(report["passed"])
        self.assertTrue(
            any(
                "feature.rs is not reachable from Rust module root lib.rs" in error
                for error in report["errors"]
            )
        )

    def test_issue_backed_semantic_waiver_clears_matching_edge(self) -> None:
        (self.root / "lib.rs").write_text("", encoding="utf-8")
        (self.root / "feature.rs").write_text("fn install() {}\n", encoding="utf-8")
        manifest, contracts = write_fixture(
            self.root,
            evidence=[
                semantic_reachability_fixture(
                    [
                        {
                            "id": "feature-module",
                            "kind": "rust_module",
                            "root": "lib.rs",
                            "path": "feature.rs",
                            "token": "",
                        }
                    ],
                    waivers=[
                        {
                            "edge": "feature-module",
                            "rationale": "tracked cleanup",
                            "issue": 664,
                            "owner": "Codex Lab",
                            "expires": "2099-01-01",
                        }
                    ],
                    max_waivers=1,
                )
            ],
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertEqual([], report["errors"])
        self.assertTrue(report["passed"])

    def test_reports_stale_semantic_waiver(self) -> None:
        (self.root / "lib.rs").write_text("mod feature;\n", encoding="utf-8")
        (self.root / "feature.rs").write_text("fn install() {}\n", encoding="utf-8")
        manifest, contracts = write_fixture(
            self.root,
            evidence=[
                semantic_reachability_fixture(
                    [
                        {
                            "id": "feature-module",
                            "kind": "rust_module",
                            "root": "lib.rs",
                            "path": "feature.rs",
                            "token": "",
                        }
                    ],
                    waivers=[
                        {
                            "edge": "feature-module",
                            "rationale": "tracked cleanup",
                            "issue": 664,
                            "owner": "Codex Lab",
                            "expires": "2099-01-01",
                        }
                    ],
                    max_waivers=1,
                )
            ],
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertFalse(report["passed"])
        self.assertIn(
            "HOME-1.evidence[0].waivers: stale waiver for reachable edge feature-module",
            report["errors"],
        )

    def test_reports_missing_symbol(self) -> None:
        manifest, contracts = write_fixture(self.root)
        (self.root / "proof.rs").write_text("fn renamed() {}\n", encoding="utf-8")

        report = self.verify_fixture(manifest, contracts)

        self.assertFalse(report["passed"])
        self.assertIn("'fn proof()' is absent from proof.rs", report["errors"][0])

    def test_reports_missing_file_evidence(self) -> None:
        manifest, contracts = write_fixture(
            self.root,
            evidence=[{"kind": "file", "path": "missing.rs", "ciTier": "nightly"}],
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertFalse(report["passed"])
        self.assertTrue(
            any("file is missing: missing.rs" in error for error in report["errors"])
        )

    def test_reports_unregistered_suite_proof(self) -> None:
        suite = self.root / "tests" / "suite"
        suite.mkdir(parents=True)
        (suite / "proof.rs").write_text("fn proof() {}\n", encoding="utf-8")
        (suite / "mod.rs").write_text("", encoding="utf-8")
        manifest, contracts = write_fixture(
            self.root,
            evidence=[
                {
                    "kind": "file",
                    "path": "tests/suite/proof.rs",
                    "ciTier": "nightly",
                }
            ],
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertFalse(report["passed"])
        self.assertTrue(
            any("suite proof is not registered" in error for error in report["errors"])
        )

    def test_reports_missing_and_extra_contract_ids(self) -> None:
        manifest, contracts = write_fixture(self.root, contract_id="AUTH-1")
        contracts.write_text(
            "| Contract | Gate |\n| --- | --- |\n| `HOME-1` | proof |\n",
            encoding="utf-8",
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertIn("manifest is missing contract IDs: ['HOME-1']", report["errors"])
        self.assertIn(
            "manifest has contract IDs absent from Markdown: ['AUTH-1']",
            report["errors"],
        )

    def test_rejects_path_escape(self) -> None:
        manifest, contracts = write_fixture(
            self.root,
            evidence=[{"kind": "file", "path": "../proof.rs", "ciTier": "nightly"}],
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertFalse(report["passed"])
        self.assertIn("path must stay inside the repository", report["errors"][0])

    def test_rejects_narrative_without_issue(self) -> None:
        manifest, contracts = write_fixture(
            self.root,
            evidence=[
                {
                    "kind": "narrative",
                    "description": "External proof.",
                    "ciTier": "release",
                }
            ],
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertFalse(report["passed"])
        self.assertIn("missing ['issue']", report["errors"][0])

    def test_rejects_narrative_only_contract(self) -> None:
        manifest, contracts = write_fixture(
            self.root,
            evidence=[
                {
                    "kind": "narrative",
                    "description": "External proof.",
                    "issue": 1,
                    "ciTier": "release",
                }
            ],
        )

        report = self.verify_fixture(manifest, contracts)

        self.assertFalse(report["passed"])
        self.assertTrue(
            any(
                "must retain at least one file-backed" in error
                for error in report["errors"]
            )
        )

    def test_rejects_unknown_schema_version(self) -> None:
        manifest, contracts = write_fixture(self.root)
        document = json.loads(manifest.read_text(encoding="utf-8"))
        document["schemaVersion"] = 2
        manifest.write_text(json.dumps(document), encoding="utf-8")

        report = self.verify_fixture(manifest, contracts)

        self.assertFalse(report["passed"])
        self.assertIn("unsupported schemaVersion 2", report["errors"][0])

    def test_cli_returns_nonzero_for_invalid_manifest(self) -> None:
        manifest, contracts = write_fixture(self.root)
        (self.root / "proof.rs").unlink()

        result = subprocess.run(
            [
                sys.executable,
                str(gates.REPO_ROOT / ".github/scripts/upstream_convergence_gates.py"),
                "--repo-root",
                str(self.root),
                "--manifest",
                str(manifest),
                "--contracts",
                str(contracts),
            ],
            check=False,
            capture_output=True,
            text=True,
        )

        self.assertNotEqual(0, result.returncode)
        self.assertIn("file is missing", result.stderr)


class CheckedInGateManifestTest(unittest.TestCase):
    def test_checked_in_manifest_resolves(self) -> None:
        report = gates.verify(
            gates.REPO_ROOT,
            gates.DEFAULT_MANIFEST,
            gates.DEFAULT_CONTRACTS,
        )

        self.assertEqual([], report["errors"])
        self.assertTrue(report["passed"])


if __name__ == "__main__":
    unittest.main()
