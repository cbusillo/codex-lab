import json
import subprocess
import tempfile
import unittest
from pathlib import Path

import upstream_convergence as convergence
import verify_upstream_convergence_governance as governance


def run(repo: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


class GitFixture(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)
        run(self.root, "init", "-b", "main")
        run(self.root, "config", "user.email", "test@example.com")
        run(self.root, "config", "user.name", "Test")
        self.policy = governance.ConvergencePolicy(
            repository="openai/codex",
            remote="openai",
            branch="main",
            allowed_fetch_urls=("https://github.com/openai/codex.git",),
            contracts_path="upstream/convergence-contracts.md",
            evidence_root="upstream/openai-codex",
            plan_issue="https://github.com/example/repo/issues/1",
        )

    def commit_file(self, path: str, contents: str, message: str) -> str:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents, encoding="utf-8")
        run(self.root, "add", path)
        run(self.root, "commit", "-m", message)
        return run(self.root, "rev-parse", "HEAD")


class RemoteIdentityTest(GitFixture):
    def test_normalizes_supported_github_urls(self) -> None:
        expected = "github.com/openai/codex"

        self.assertEqual(
            expected,
            convergence.normalize_remote_url("git@github.com:openai/codex.git"),
        )
        self.assertEqual(
            expected,
            convergence.normalize_remote_url("https://github.com/openai/codex.git"),
        )

    def test_rejects_remote_substitution(self) -> None:
        run(self.root, "remote", "add", "openai", "https://github.com/evil/fork.git")

        with self.assertRaises(convergence.ConvergenceError):
            convergence.remote_identity(self.root, self.policy)


class SnapshotMutationTest(GitFixture):
    def test_allows_new_snapshot_directory(self) -> None:
        base = self.commit_file(
            "upstream/openai-codex/aaaaaaaa-bbbbbbbb/inventory.json",
            "{}\n",
            "base snapshot",
        )
        run(self.root, "switch", "-c", "task")
        self.commit_file(
            "upstream/openai-codex/cccccccc-dddddddd/inventory.json",
            "{}\n",
            "new snapshot",
        )

        self.assertEqual(
            [], convergence.snapshot_change_errors(self.root, self.policy, base)
        )

    def test_rejects_historical_snapshot_modification(self) -> None:
        path = "upstream/openai-codex/aaaaaaaa-bbbbbbbb/inventory.json"
        base = self.commit_file(path, "{}\n", "base snapshot")
        run(self.root, "switch", "-c", "task")
        self.commit_file(path, '{"changed": true}\n', "rewrite snapshot")

        self.assertEqual(
            [f"historical snapshot changed (M): {path}"],
            convergence.snapshot_change_errors(self.root, self.policy, base),
        )


class RecordedUpstreamTest(GitFixture):
    def write_snapshot(self, name: str, upstream: str) -> None:
        directory = self.root / self.policy.evidence_root / name
        directory.mkdir(parents=True)
        (directory / "inventory.json").write_text(
            json.dumps({"refs": {"upstream": upstream}}), encoding="utf-8"
        )

    def test_rejects_backward_upstream_target(self) -> None:
        older = self.commit_file("one.txt", "one\n", "one")
        newer = self.commit_file("two.txt", "two\n", "two")
        self.write_snapshot("older", older)
        self.write_snapshot("newer", newer)

        with self.assertRaises(convergence.ConvergenceError):
            convergence.require_forward_upstream(self.root, self.policy, older)

    def test_accepts_descendant_of_recorded_tip(self) -> None:
        older = self.commit_file("one.txt", "one\n", "one")
        self.write_snapshot("older", older)
        newer = self.commit_file("two.txt", "two\n", "two")

        self.assertEqual(
            older,
            convergence.require_forward_upstream(self.root, self.policy, newer),
        )


class ExactRefTest(GitFixture):
    def test_rejects_symbolic_ref(self) -> None:
        self.commit_file("README.md", "test\n", "initial")

        with self.assertRaises(convergence.ConvergenceError):
            convergence.resolve_exact_commit(self.root, "HEAD", "local")


class CheckedInSnapshotTest(unittest.TestCase):
    def test_checked_in_snapshots_are_valid(self) -> None:
        policy = governance.load_policy(
            convergence.POLICY_PATH, convergence.REPO_ROOT
        )

        report = convergence.validate_snapshots(convergence.REPO_ROOT, policy)

        self.assertEqual([], report["errors"])
        self.assertTrue(report["passed"])
        self.assertGreaterEqual(report["count"], 1)


if __name__ == "__main__":
    unittest.main()
