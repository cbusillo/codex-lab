import json
import shutil
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


def loose_object_count(repo: Path) -> int:
    fields = dict(
        line.split(": ", 1)
        for line in run(repo, "count-objects", "-v").splitlines()
        if ": " in line
    )
    return int(fields["count"])


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
        self.assertEqual(
            "ssh://git@github.com/openai/codex",
            convergence.normalize_remote_url("git@github.com:openai/codex.git"),
        )
        self.assertEqual(
            "https://github.com/openai/codex",
            convergence.normalize_remote_url("https://github.com/openai/codex.git"),
        )

    def test_rejects_remote_substitution(self) -> None:
        run(self.root, "remote", "add", "openai", "https://github.com/evil/fork.git")

        with self.assertRaises(convergence.ConvergenceError):
            convergence.remote_identity(self.root, self.policy)

    def test_rejects_embedded_remote_credentials(self) -> None:
        with self.assertRaisesRegex(convergence.ConvergenceError, "credentials"):
            convergence.normalize_remote_url(
                "https://secret@github.com/openai/codex.git"
            )

    def test_preserves_nondefault_port_in_remote_identity(self) -> None:
        self.assertEqual(
            "https://github.com:444/openai/codex",
            convergence.normalize_remote_url(
                "https://github.com:444/openai/codex.git"
            ),
        )

    def test_rejects_insecure_remote_transport(self) -> None:
        with self.assertRaisesRegex(convergence.ConvergenceError, "unsupported"):
            convergence.normalize_remote_url("http://github.com/openai/codex.git")


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


class RecordIntegrationTest(GitFixture):
    def make_linked_candidate(self) -> tuple[Path, str, str, str]:
        base = self.commit_file("base.txt", "base\n", "base")
        run(self.root, "switch", "-c", "local")
        local = self.commit_file("local.txt", "local\n", "local")
        run(self.root, "switch", "-c", "upstream", base)
        upstream = self.commit_file("upstream.txt", "upstream\n", "upstream")
        run(
            self.root,
            "remote",
            "add",
            "openai",
            "https://github.com/openai/codex.git",
        )
        run(self.root, "update-ref", "refs/remotes/openai/main", upstream)

        linked = Path(tempfile.mkdtemp(prefix="convergence-record-"))
        linked.rmdir()

        def cleanup() -> None:
            subprocess.run(
                ["git", "-C", str(self.root), "worktree", "remove", "--force", str(linked)],
                capture_output=True,
                text=True,
            )
            shutil.rmtree(linked, ignore_errors=True)

        self.addCleanup(cleanup)
        run(self.root, "worktree", "add", "-b", "task", str(linked), local)
        run(linked, "merge", "--no-ff", "upstream", "-m", "merge upstream")
        return linked, base, upstream, local

    def test_records_atomically_and_refuses_overwrite(self) -> None:
        linked, base, upstream, local = self.make_linked_candidate()
        objects_before = loose_object_count(linked)
        review_base = run(linked, "rev-parse", "HEAD")

        report = convergence.record(
            linked,
            self.policy,
            base,
            upstream,
            local,
        )

        snapshot = linked / str(report["snapshot"])
        self.assertEqual(objects_before, loose_object_count(linked))
        self.assertEqual(0, report["existingSnapshotsValidated"])
        self.assertEqual(
            sorted(convergence.SNAPSHOT_FILES),
            sorted(path.name for path in snapshot.iterdir()),
        )
        document = json.loads((snapshot / "inventory.json").read_text(encoding="utf-8"))
        self.assertEqual(convergence.inventory.POLICY_VERSION, document["policy"]["version"])
        run(linked, "add", str(snapshot.relative_to(linked)))
        run(linked, "commit", "-m", "record snapshot")
        change_errors, added, existing = convergence.snapshot_change_analysis(
            linked, self.policy, review_base
        )
        self.assertEqual([], change_errors)
        self.assertEqual({snapshot.name}, added)
        self.assertEqual(set(), existing)
        self.assertEqual(
            [],
            convergence.validate_new_snapshot_provenance(
                linked,
                self.policy,
                review_base,
                upstream,
                added,
                existing,
            ),
        )
        with self.assertRaisesRegex(convergence.ConvergenceError, "snapshot already exists"):
            convergence.record(linked, self.policy, base, upstream, local)


class LockTest(GitFixture):
    def test_lock_clears_diagnostic_owner_after_release(self) -> None:
        lock_path = convergence.git_common_dir(self.root) / "upstream-convergence.lock"

        with convergence.convergence_lock(self.root):
            self.assertIn(b"pid=", lock_path.read_bytes())

        self.assertEqual(b"\0", lock_path.read_bytes())

    def test_second_lock_holder_is_rejected(self) -> None:
        with convergence.convergence_lock(self.root):
            with self.assertRaises(convergence.ConvergenceError):
                with convergence.convergence_lock(self.root):
                    self.fail("second lock holder should not enter")


class ExactRefTest(GitFixture):
    def test_rejects_symbolic_ref(self) -> None:
        self.commit_file("README.md", "test\n", "initial")

        with self.assertRaises(convergence.ConvergenceError):
            convergence.resolve_exact_commit(self.root, "HEAD", "local")


class SafetyStateTest(unittest.TestCase):
    def state(self, **overrides: object) -> dict[str, object]:
        state: dict[str, object] = {
            "clean": True,
            "operationMarkers": [],
            "replacementRefs": [],
            "shallow": False,
        }
        state.update(overrides)
        return state

    def test_inspection_rejects_shallow_history(self) -> None:
        with self.assertRaisesRegex(convergence.ConvergenceError, "complete Git history"):
            convergence.require_read_safety(self.state(shallow=True))

    def test_inspection_rejects_replacement_refs(self) -> None:
        with self.assertRaisesRegex(convergence.ConvergenceError, "replacement refs"):
            convergence.require_read_safety(
                self.state(replacementRefs=["refs/replace/abc"])
            )


class SnapshotAvailabilityTest(GitFixture):
    def write_snapshot(self) -> Path:
        snapshot = (
            self.root
            / self.policy.evidence_root
            / "aaaaaaaa-bbbbbbbb"
        )
        snapshot.mkdir(parents=True)
        (snapshot / "inventory.json").write_text(
            json.dumps(
                {
                    "repository": "openai/codex",
                    "refs": {
                        "base": "a" * 40,
                        "upstream": "b" * 40,
                        "local": "c" * 40,
                    },
                    "policy": {
                        "defaultLane": "green_bulk_adopt",
                        "rule": "Upstream wins.",
                    },
                }
            ),
            encoding="utf-8",
        )
        (snapshot / "inventory.md").write_text("# Inventory\n", encoding="utf-8")
        (snapshot / "residuals.json").write_text("{}\n", encoding="utf-8")
        return snapshot

    def test_reports_missing_history_without_claiming_reproduction(self) -> None:
        snapshot = self.write_snapshot()

        report = convergence.validate_snapshots(self.root, self.policy)

        self.assertTrue(report["passed"])
        self.assertEqual([], report["reproduced"])
        self.assertEqual(
            [str(snapshot.relative_to(self.root))], report["historyUnavailable"]
        )

    def test_rejects_symbolic_links_inside_snapshot(self) -> None:
        snapshot = self.write_snapshot()
        target = self.root / "outside-inventory.json"
        target.write_text("{}\n", encoding="utf-8")
        (snapshot / "inventory.json").unlink()
        (snapshot / "inventory.json").symlink_to(target)

        report = convergence.validate_snapshots(self.root, self.policy)

        self.assertFalse(report["passed"])
        self.assertIn("symbolic links", report["errors"][0])

    def test_rejects_non_object_inventory(self) -> None:
        snapshot = self.write_snapshot()
        (snapshot / "inventory.json").write_text("[]\n", encoding="utf-8")

        report = convergence.validate_snapshots(self.root, self.policy)

        self.assertFalse(report["passed"])
        self.assertIn("must contain a JSON object", report["errors"][0])

    def test_rejects_non_snapshot_entry_in_evidence_root(self) -> None:
        root = self.root / self.policy.evidence_root
        root.mkdir(parents=True)
        (root / "README.txt").write_text("unexpected\n", encoding="utf-8")

        report = convergence.validate_snapshots(self.root, self.policy)

        self.assertFalse(report["passed"])
        self.assertIn("non-snapshot entries", report["errors"][0])

    def test_rejects_symlinked_evidence_root(self) -> None:
        target = self.root / "other-evidence"
        target.mkdir()
        evidence_root = self.root / self.policy.evidence_root
        evidence_root.parent.mkdir(parents=True)
        evidence_root.symlink_to(target, target_is_directory=True)

        report = convergence.validate_snapshots(self.root, self.policy)

        self.assertFalse(report["passed"])
        self.assertIn("evidence root must not be a symlink", report["errors"][0])


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
