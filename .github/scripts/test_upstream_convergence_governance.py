import json
import tempfile
import unittest
from pathlib import Path

import verify_upstream_convergence_governance as governance


def policy_document(**overrides: object) -> dict[str, object]:
    document: dict[str, object] = {
        "schemaVersion": 1,
        "upstream": {
            "repository": "openai/codex",
            "remote": "openai",
            "branch": "main",
            "allowedFetchUrls": ["https://github.com/openai/codex.git"],
        },
        "contractsPath": "upstream/convergence-contracts.md",
        "evidenceRoot": "upstream/openai-codex",
        "planIssue": "https://github.com/example/repo/issues/1",
    }
    document.update(overrides)
    return document


class PolicyValidationTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        (self.root / "upstream").mkdir()
        self.path = self.root / "upstream" / "convergence-policy.json"

    def write(self, document: dict[str, object]) -> None:
        self.path.write_text(json.dumps(document), encoding="utf-8")

    def test_loads_minimal_identity_policy(self) -> None:
        self.write(policy_document())

        policy = governance.load_policy(self.path, self.root)

        self.assertEqual("openai/codex", policy.repository)
        self.assertEqual("upstream/openai-codex", policy.evidence_root)

    def test_rejects_repository_path_escape(self) -> None:
        self.write(policy_document(contractsPath="../contracts.md"))

        with self.assertRaises(governance.PolicyError):
            governance.load_policy(self.path, self.root)

    def test_rejects_unknown_policy_fields(self) -> None:
        self.write(policy_document(command=["arbitrary", "hook"]))

        with self.assertRaises(governance.PolicyError):
            governance.load_policy(self.path, self.root)

    def test_rejects_empty_allowed_url_list(self) -> None:
        document = policy_document()
        document["upstream"]["allowedFetchUrls"] = []
        self.write(document)

        with self.assertRaises(governance.PolicyError):
            governance.load_policy(self.path, self.root)

    def test_rejects_policy_file_outside_repository(self) -> None:
        outside = self.root.parent / f"{self.root.name}-policy.json"
        outside.write_text(json.dumps(policy_document()), encoding="utf-8")
        self.addCleanup(outside.unlink)

        with self.assertRaises(governance.PolicyError):
            governance.load_policy(outside, self.root)

    def test_missing_governance_files_are_reported_without_crashing(self) -> None:
        self.write(policy_document())

        report = governance.verify(self.root, self.path)

        self.assertFalse(report["passed"])
        self.assertIn(
            "required governance file is missing: AGENTS.md", report["errors"]
        )

    def test_rejects_symlinked_governance_file(self) -> None:
        self.write(policy_document())
        target = self.root / "agents-target.md"
        target.write_text("# Target\n", encoding="utf-8")
        (self.root / "AGENTS.md").symlink_to(target)

        report = governance.verify(self.root, self.path)

        self.assertIn(
            "required governance file must not be a symlink: AGENTS.md",
            report["errors"],
        )

    def test_reports_policy_identity_change(self) -> None:
        self.write(policy_document())

        report = governance.verify(self.root, self.path)

        self.assertIn(
            "convergence policy differs from the pinned Codex Lab identity",
            report["errors"],
        )


class CheckedInGovernanceTest(unittest.TestCase):
    def test_repository_governance_is_complete_and_owned(self) -> None:
        report = governance.verify(governance.REPO_ROOT, governance.DEFAULT_POLICY)

        self.assertEqual([], report["errors"])
        self.assertTrue(report["passed"])
        self.assertTrue(report["guardBaselineReproduced"])


if __name__ == "__main__":
    unittest.main()
