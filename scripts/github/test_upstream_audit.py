import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import upstream_audit


SCRIPT = Path(__file__).with_name("upstream_audit.py")
TIMEOUT_SECONDS = 30


def git(repo: Path, *arguments: str, environment: dict[str, str] | None = None) -> str:
    command_environment = os.environ.copy()
    command_environment.update(environment or {})
    completed = subprocess.run(
        ["git", *arguments],
        cwd=repo,
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=TIMEOUT_SECONDS,
        env=command_environment,
    )
    if completed.returncode != 0:
        raise AssertionError(
            f"git {' '.join(arguments)} failed:\n{completed.stdout}{completed.stderr}"
        )
    return completed.stdout.strip()


def configure(repo: Path) -> None:
    git(repo, "config", "user.name", "Audit Fixture")
    git(repo, "config", "user.email", "audit-fixture@example.com")


def commit(repo: Path, path: str, content: str, message: str, date: str) -> str:
    target = repo / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content, encoding="utf-8")
    git(repo, "add", "--", path)
    git(
        repo,
        "commit",
        "--no-gpg-sign",
        "-m",
        message,
        environment={"GIT_AUTHOR_DATE": date, "GIT_COMMITTER_DATE": date},
    )
    return git(repo, "rev-parse", "HEAD")


class Fixture:
    def __init__(self, root: Path, object_format: str = "sha1") -> None:
        self.root = root
        self.upstream = root / "upstream"
        self.implementation = root / "implementation"
        self.upstream.mkdir()
        git(self.upstream, "init", f"--object-format={object_format}", "-b", "main")
        configure(self.upstream)
        self.base = commit(
            self.upstream, "README.md", "base\n", "base", "2026-07-18T12:00:00Z"
        )
        git(root, "clone", str(self.upstream), str(self.implementation))
        configure(self.implementation)
        self.upstream_core = commit(
            self.upstream,
            "codex-rs/core/src/upstream.rs",
            'pub const VALUE: &str = "shared";\n',
            "upstream core",
            "2026-07-19T12:00:00Z",
        )
        self.baseline = commit(
            self.implementation,
            "codex-rs/core/src/upstream.rs",
            'pub const VALUE: &str = "shared";\n',
            "adapt core",
            "2026-07-19T13:00:00Z",
        )
        self.head = commit(
            self.upstream,
            "codex-rs/tui/src/upstream.rs",
            'pub const VALUE: &str = "missing";\n',
            "upstream tui",
            "2026-07-20T12:00:00Z",
        )
        (self.implementation / "README.md").write_text(
            "base\ndirty\n", encoding="utf-8"
        )

    def run(
        self,
        *,
        checkpoint: str | None = None,
        adopted: str | None = None,
        upstream: Path | None = None,
        repo: Path | None = None,
        baseline: str | None = None,
        max_commits: int = 10,
        environment: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        arguments = [
            sys.executable,
            str(SCRIPT),
            "--repo",
            str(repo or self.implementation),
            "--upstream-url",
            str(upstream or self.upstream),
            "--upstream-branch",
            "main",
            "--implementation-baseline",
            baseline or self.baseline,
            "--classified-checkpoint",
            checkpoint or self.base,
            "--command-timeout-seconds",
            str(TIMEOUT_SECONDS),
            "--max-commits",
            str(max_commits),
        ]
        if adopted:
            arguments.extend(["--adopted-checkpoint", adopted])
        command_environment = os.environ.copy()
        command_environment.update(environment or {})
        return subprocess.run(
            arguments,
            cwd=self.root,
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=TIMEOUT_SECONDS,
            env=command_environment,
        )


def source_state(repo: Path) -> dict[str, str]:
    return {
        "config": git(repo, "config", "--local", "--list"),
        "index": git(repo, "ls-files", "--stage"),
        "refs": git(repo, "for-each-ref", "--format=%(refname) %(objectname)"),
        "status": git(repo, "status", "--porcelain=v2", "--branch"),
    }


class UpstreamAuditTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.fixture = Fixture(Path(self.temp_dir.name))

    def test_collects_deterministic_evidence_without_source_writes(self) -> None:
        before = source_state(self.fixture.implementation)
        first = self.fixture.run(
            checkpoint=self.fixture.upstream_core,
            environment={"GIT_DIR": str(self.fixture.upstream / ".git")},
        )
        second = self.fixture.run(checkpoint=self.fixture.upstream_core)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout, second.stdout)
        core_buckets = {bucket: 0 for bucket in upstream_audit.BUCKET_ORDER}
        core_buckets["core"] = 1
        tui_buckets = {bucket: 0 for bucket in upstream_audit.BUCKET_ORDER}
        tui_buckets["tui"] = 1
        self.assertEqual(
            json.loads(first.stdout),
            {
                "implementation": {
                    "baseline": self.fixture.baseline,
                    "mergeBaseWithClassifiedCheckpoint": self.fixture.base,
                    "mergeBaseWithObservedUpstream": self.fixture.base,
                },
                "ranges": {
                    "postCheckpoint": {
                        "after": self.fixture.upstream_core,
                        "commitCount": 1,
                        "commits": [
                            {
                                "mechanicalStatus": "missing_patch",
                                "primaryPathBucket": "tui",
                                "sha": self.fixture.head,
                            }
                        ],
                        "patchEquivalence": {
                            "algorithm": "git-reachability-and-cherry-v1",
                            "equivalenceScope": "exact_commit_or-git-cherry-patch-only",
                            "exactCommitCount": 0,
                            "missingPatchCount": 1,
                            "patchEquivalentCommitCount": 0,
                            "uncomparableCommitCount": 0,
                        },
                        "primaryPathBuckets": tui_buckets,
                        "through": self.fixture.head,
                    },
                    "preCheckpoint": {
                        "after": self.fixture.base,
                        "commitCount": 1,
                        "commits": [
                            {
                                "mechanicalStatus": "patch_equivalent",
                                "primaryPathBucket": "core",
                                "sha": self.fixture.upstream_core,
                            }
                        ],
                        "patchEquivalence": {
                            "algorithm": "git-reachability-and-cherry-v1",
                            "equivalenceScope": "exact_commit_or-git-cherry-patch-only",
                            "exactCommitCount": 0,
                            "missingPatchCount": 0,
                            "patchEquivalentCommitCount": 1,
                            "uncomparableCommitCount": 0,
                        },
                        "primaryPathBuckets": core_buckets,
                        "through": self.fixture.upstream_core,
                    },
                },
                "schemaVersion": 2,
                "upstream": {
                    "adoptedCheckpoint": None,
                    "branch": "main",
                    "classifiedCheckpoint": self.fixture.upstream_core,
                    "observedHead": self.fixture.head,
                },
            },
        )
        self.assertEqual(source_state(self.fixture.implementation), before)

    def test_empty_post_range_keeps_the_complete_schema(self) -> None:
        completed = self.fixture.run(
            checkpoint=self.fixture.head, adopted=self.fixture.base
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        payload = json.loads(completed.stdout)
        self.assertEqual(
            payload["ranges"]["postCheckpoint"],
            {
                "after": self.fixture.head,
                "commitCount": 0,
                "commits": [],
                "patchEquivalence": {
                    "algorithm": "git-reachability-and-cherry-v1",
                    "equivalenceScope": "exact_commit_or-git-cherry-patch-only",
                    "exactCommitCount": 0,
                    "missingPatchCount": 0,
                    "patchEquivalentCommitCount": 0,
                    "uncomparableCommitCount": 0,
                },
                "primaryPathBuckets": {
                    bucket: 0 for bucket in upstream_audit.BUCKET_ORDER
                },
                "through": self.fixture.head,
            },
        )

    def test_bounded_failures_are_actionable_and_redacted(self) -> None:
        cases = (
            (
                self.fixture.run(checkpoint="f" * 40),
                "classified checkpoint is not available",
            ),
            (
                self.fixture.run(upstream=self.fixture.root / "missing.git"),
                "failed to fetch live upstream head",
            ),
            (self.fixture.run(max_commits=1), "exceeding the configured maximum of 1"),
        )
        for completed, expected in cases:
            with self.subTest(expected=expected):
                self.assertNotEqual(completed.returncode, 0)
                self.assertEqual(completed.stdout, "")
                self.assertIn(expected, completed.stderr)
                self.assertNotIn(str(self.fixture.root), completed.stderr)

    def test_counts_exactly_reachable_upstream_commits(self) -> None:
        git(
            self.fixture.implementation,
            "fetch",
            str(self.fixture.upstream),
            self.fixture.upstream_core,
        )
        completed = self.fixture.run(baseline=self.fixture.upstream_core)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        payload = json.loads(completed.stdout)
        self.assertEqual(
            payload["implementation"],
            {
                "baseline": self.fixture.upstream_core,
                "mergeBaseWithClassifiedCheckpoint": self.fixture.base,
                "mergeBaseWithObservedUpstream": self.fixture.upstream_core,
            },
        )
        ranges = payload["ranges"]
        self.assertEqual(
            (ranges["preCheckpoint"]["after"], ranges["preCheckpoint"]["through"]),
            (self.fixture.base, self.fixture.base),
        )
        delta = ranges["postCheckpoint"]
        self.assertEqual(
            delta["patchEquivalence"],
            {
                "algorithm": "git-reachability-and-cherry-v1",
                "equivalenceScope": "exact_commit_or-git-cherry-patch-only",
                "exactCommitCount": 1,
                "missingPatchCount": 1,
                "patchEquivalentCommitCount": 0,
                "uncomparableCommitCount": 0,
            },
        )
        self.assertEqual(
            delta["commits"],
            [
                {
                    "mechanicalStatus": "exact_commit",
                    "primaryPathBucket": "core",
                    "sha": self.fixture.upstream_core,
                },
                {
                    "mechanicalStatus": "missing_patch",
                    "primaryPathBucket": "tui",
                    "sha": self.fixture.head,
                },
            ],
        )

    def test_rejects_invalid_checkpoint_order(self) -> None:
        git(self.fixture.upstream, "switch", "--orphan", "unrelated")
        unrelated = commit(
            self.fixture.upstream,
            "unrelated.txt",
            "unrelated\n",
            "unrelated",
            "2026-07-20T13:00:00Z",
        )
        git(self.fixture.upstream, "switch", "main")
        git(self.fixture.implementation, "fetch", str(self.fixture.upstream), unrelated)
        cases = (
            (self.fixture.run(checkpoint=unrelated), "is not an ancestor"),
            (
                self.fixture.run(adopted=self.fixture.head),
                "adopted checkpoint is not an ancestor of the classified checkpoint",
            ),
        )
        for completed, expected in cases:
            with self.subTest(expected=expected):
                self.assertNotEqual(completed.returncode, 0)
                self.assertIn(expected, completed.stderr)

    def test_rejects_shallow_and_promisor_source_repositories(self) -> None:
        shallow = self.fixture.root / "shallow"
        git(
            self.fixture.root,
            "clone",
            "--depth",
            "1",
            f"file://{self.fixture.upstream}",
            str(shallow),
        )
        shallow_result = self.fixture.run(repo=shallow)
        git(self.fixture.implementation, "config", "remote.origin.promisor", "true")
        promisor_result = self.fixture.run()
        for completed, expected in (
            (shallow_result, "repository is shallow"),
            (promisor_result, "repository is a promisor clone"),
        ):
            with self.subTest(expected=expected):
                self.assertNotEqual(completed.returncode, 0)
                self.assertIn(expected, completed.stderr)

    def test_supports_sha256_repositories(self) -> None:
        root = Path(self.temp_dir.name) / "sha256"
        root.mkdir()
        completed = Fixture(root, object_format="sha256").run()
        self.assertEqual(completed.returncode, 0, completed.stderr)
