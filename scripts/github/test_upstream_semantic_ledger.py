import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import upstream_semantic_ledger


BASELINE = "a" * 40
CLASSIFIED = "b" * 40
OBSERVED = "c" * 40
CLASSIFIED_MERGE_BASE = "d" * 40
OBSERVED_MERGE_BASE = "e" * 40
COMMITS = ("1" * 40, "2" * 40, OBSERVED)
PRE_COMMITS = ("3" * 40, "4" * 40, CLASSIFIED)
SCRIPT = Path(__file__).with_name("upstream_semantic_ledger.py")


def range_payload(
    after: str,
    through: str,
    commits: tuple[str, ...],
    statuses: tuple[str, ...],
    commit_buckets: tuple[str, ...],
) -> dict[str, object]:
    buckets = {bucket: 0 for bucket in upstream_semantic_ledger.AUDIT_BUCKETS}
    for bucket in commit_buckets:
        buckets[bucket] += 1
    return {
        "after": after,
        "commitCount": len(commits),
        "commits": [
            {
                "mechanicalStatus": status,
                "primaryPathBucket": bucket,
                "sha": commit,
            }
            for commit, status, bucket in zip(
                commits, statuses, commit_buckets, strict=True
            )
        ],
        "patchEquivalence": {
            "algorithm": "git-reachability-and-cherry-v1",
            "equivalenceScope": "exact_commit_or-git-cherry-patch-only",
            "exactCommitCount": statuses.count("exact_commit"),
            "missingPatchCount": statuses.count("missing_patch"),
            "patchEquivalentCommitCount": statuses.count("patch_equivalent"),
            "uncomparableCommitCount": statuses.count("uncomparable"),
        },
        "primaryPathBuckets": buckets,
        "through": through,
    }


def audit_payload() -> dict[str, object]:
    return {
        "implementation": {
            "baseline": BASELINE,
            "mergeBaseWithClassifiedCheckpoint": CLASSIFIED_MERGE_BASE,
            "mergeBaseWithObservedUpstream": OBSERVED_MERGE_BASE,
        },
        "ranges": {
            "postCheckpoint": range_payload(
                CLASSIFIED,
                OBSERVED,
                COMMITS,
                ("exact_commit", "patch_equivalent", "missing_patch"),
                ("core", "core", "tui"),
            ),
            "preCheckpoint": range_payload(
                CLASSIFIED_MERGE_BASE,
                CLASSIFIED,
                PRE_COMMITS,
                ("patch_equivalent", "uncomparable", "missing_patch"),
                ("app_server", "core", "tui"),
            ),
        },
        "schemaVersion": 2,
        "upstream": {
            "adoptedCheckpoint": None,
            "branch": "main",
            "classifiedCheckpoint": CLASSIFIED,
            "observedHead": OBSERVED,
        },
    }


def ledger_payload(*, window: str = "post_checkpoint") -> dict[str, object]:
    if window == "pre_checkpoint":
        commits = PRE_COMMITS
        after = CLASSIFIED_MERGE_BASE
        through = CLASSIFIED
        dispositions = ("adopted", "adapted")
    else:
        commits = COMMITS
        after = CLASSIFIED
        through = OBSERVED
        dispositions = ("adopt", "adapt")
    return {
        "kind": upstream_semantic_ledger.LEDGER_KIND,
        "range": {
            "after": after,
            "implementationBaseline": BASELINE,
            "through": through,
            "window": window,
        },
        "rows": [
            {
                "area": "core",
                "commits": [commits[0]],
                "disposition": dispositions[0],
                "evidence": [
                    {
                        "kind": "github_pull_request",
                        "reference": "cbusillo/codex-lab#402",
                    }
                ],
                "implementationStatus": "implemented",
                "summary": "Adopted directly.",
            },
            {
                "area": "cross_cutting",
                "commits": list(commits[1:]),
                "disposition": dispositions[1],
                "evidence": [
                    {"kind": "github_issue", "reference": "cbusillo/codex-lab#403"}
                ],
                "implementationStatus": "implemented",
                "stackId": "bounded-stack",
                "summary": "Adapted as one bounded stack.",
            },
        ],
        "schemaVersion": 1,
        "upstream": {"branch": "main", "repository": "openai/codex"},
    }


def review_payload(ledger: dict[str, object] | None = None) -> dict[str, object]:
    ledger = ledger or ledger_payload()
    row_keys = [row.get("stackId", row["commits"][0]) for row in ledger["rows"]]
    return {
        "crossStackDependencies": [
            {
                "dependsOn": ["openai/codex#402"],
                "from": row_keys[-1],
                "reason": "The bounded stack follows the direct adoption.",
                "sourceReview": "core_a",
            }
        ],
        "issue": "cbusillo/codex-lab#408",
        "kind": upstream_semantic_ledger.REVIEW_KIND,
        "range": ledger["range"],
        "reviewerNotes": [
            {
                "note": "Preserve Every Code behavior while adapting upstream work.",
                "sourceReview": "core_a",
            }
        ],
        "riskFlags": [
            {
                "priority": "P1",
                "reason": "The stack changes model-visible behavior.",
                "rowKey": row_keys[-1],
            }
        ],
        "rows": [
            {
                "confidence": "high",
                "rationale": "The disposition matches the current implementation.",
                "rowKey": row_key,
                "sourceReview": "core_a",
            }
            for row_key in row_keys
        ],
        "schemaVersion": upstream_semantic_ledger.REVIEW_SCHEMA_VERSION,
    }


class UpstreamSemanticLedgerTests(unittest.TestCase):
    def test_complete_ledger_reconciles_and_renders_deterministically(self) -> None:
        audit = upstream_semantic_ledger.validate_audit(audit_payload())
        summary, rows = upstream_semantic_ledger.validate_ledger(
            ledger_payload(), audit
        )
        self.assertEqual(
            (
                summary["range"]["complete"],
                summary["range"]["classifiedCommitCount"],
                summary["range"]["missingCommitCount"],
                summary["range"]["blockingCommitCount"],
            ),
            (True, 3, 0, 0),
        )
        self.assertEqual(
            {
                key: value
                for key, value in summary["range"]["dispositions"].items()
                if value
            },
            {"adopt": 1, "adapt": 2},
        )
        first = upstream_semantic_ledger.render(summary, rows)
        self.assertEqual(first, upstream_semantic_ledger.render(summary, rows))
        self.assertIn("`bounded-stack`", first)
        self.assertIn(f"`{COMMITS[0][:12]}:exact_commit/core`", first)
        self.assertIn(f"`{COMMITS[2][:12]}:missing_patch/tui`", first)
        self.assertEqual(summary["kind"], upstream_semantic_ledger.SUMMARY_KIND)
        self.assertEqual(summary["range"]["implementationBaseline"], BASELINE)

    def test_pre_checkpoint_uses_its_own_range(self) -> None:
        audit = upstream_semantic_ledger.validate_audit(audit_payload())
        summary, rows = upstream_semantic_ledger.validate_ledger(
            ledger_payload(window="pre_checkpoint"), audit
        )
        self.assertEqual(
            (
                summary["range"]["after"],
                summary["range"]["through"],
                summary["range"]["auditCommitCount"],
                summary["range"]["complete"],
            ),
            (CLASSIFIED_MERGE_BASE, CLASSIFIED, 3, True),
        )
        rendered = upstream_semantic_ledger.render(summary, rows)
        self.assertIn(f"`{PRE_COMMITS[1][:12]}:uncomparable/core`", rendered)

    def test_missing_and_blocking_commits_are_reported(self) -> None:
        payload = ledger_payload()
        payload["rows"] = [payload["rows"][0]]
        payload["rows"][0]["disposition"] = "decision_required"
        audit = upstream_semantic_ledger.validate_audit(audit_payload())
        summary, _ = upstream_semantic_ledger.validate_ledger(payload, audit)
        self.assertEqual(summary["range"]["missingCommits"], list(COMMITS[1:]))
        self.assertEqual(summary["range"]["blockingCommits"], [COMMITS[0]])
        self.assertFalse(summary["range"]["complete"])
        pre_payload = ledger_payload(window="pre_checkpoint")
        pre_payload["rows"] = [pre_payload["rows"][0]]
        pre_payload["rows"][0]["disposition"] = "missing"
        pre_summary = upstream_semantic_ledger.validate_ledger(pre_payload, audit)[0]
        self.assertEqual(
            (
                pre_summary["range"]["missingCommits"],
                pre_summary["range"]["blockingCommits"],
            ),
            (list(PRE_COMMITS[1:]), [PRE_COMMITS[0]]),
        )

    def test_render_bounds_missing_commit_list(self) -> None:
        audit = upstream_semantic_ledger.validate_audit(audit_payload())
        payload = ledger_payload()
        payload["rows"] = [payload["rows"][0]]
        summary, rows = upstream_semantic_ledger.validate_ledger(payload, audit)
        missing = [
            f"{index:040x}"
            for index in range(upstream_semantic_ledger.MAX_RENDERED_STATUS_COMMITS + 2)
        ]
        summary["range"]["missingCommits"] = missing
        summary["range"]["missingCommitCount"] = len(missing)

        rendered = upstream_semantic_ledger.render(summary, rows)

        self.assertIn(f"- `{missing[-3]}`", rendered)
        self.assertNotIn(f"- `{missing[-2]}`", rendered)
        self.assertIn(
            "_2 additional commits omitted; run `validate` for the complete machine-readable list._",
            rendered,
        )

    def test_review_reconciles_with_ledger(self) -> None:
        audit = upstream_semantic_ledger.validate_audit(audit_payload())
        summary, rows = upstream_semantic_ledger.validate_ledger(
            ledger_payload(), audit
        )
        review = upstream_semantic_ledger.validate_review(
            review_payload(), summary, rows
        )
        self.assertEqual(
            review,
            {"dependencyCount": 1, "riskFlagCount": 1, "rowCount": 2},
        )

    def test_rejects_review_drift(self) -> None:
        audit = upstream_semantic_ledger.validate_audit(audit_payload())
        summary, rows = upstream_semantic_ledger.validate_ledger(
            ledger_payload(), audit
        )
        wrong_row = review_payload()
        wrong_row["rows"][0]["rowKey"] = "wrong-row"
        string_dependency = review_payload()
        string_dependency["crossStackDependencies"][0]["dependsOn"] = "openai/codex#402"
        unknown_source = review_payload()
        unknown_source["crossStackDependencies"][0]["from"] = "unknown-stack"
        unknown_target = review_payload()
        unknown_target["crossStackDependencies"][0]["dependsOn"] = ["unknown-stack"]
        duplicate_risk = review_payload()
        duplicate_risk["riskFlags"].append(duplicate_risk["riskFlags"][0])
        for payload, expected in (
            (wrong_row, "same position"),
            (string_dependency, "between 1 and 25"),
            (unknown_source, "must resolve"),
            (unknown_target, r"dependsOn\[0\]: must resolve"),
            (duplicate_risk, "one unique ledger row"),
        ):
            with self.subTest(expected=expected):
                with self.assertRaisesRegex(
                    upstream_semantic_ledger.LedgerError, expected
                ):
                    upstream_semantic_ledger.validate_review(payload, summary, rows)

    def test_rejects_invalid_rows_and_evidence(self) -> None:
        outside = ledger_payload()
        outside["rows"][0]["commits"] = ["f" * 40]
        overlap = ledger_payload()
        overlap["rows"][1]["commits"] = [COMMITS[0], COMMITS[1]]
        invalid_evidence = ledger_payload()
        invalid_evidence["rows"][0]["evidence"][0]["reference"] = "not-a-reference"
        unknown = ledger_payload()
        unknown["rows"][0]["unexpected"] = True
        audit = upstream_semantic_ledger.validate_audit(audit_payload())
        for payload, expected in (
            (outside, "in-range"),
            (overlap, "non-overlapping"),
            (invalid_evidence, "invalid for evidence"),
            (unknown, "unknown"),
        ):
            with self.subTest(expected=expected):
                with self.assertRaisesRegex(
                    upstream_semantic_ledger.LedgerError, expected
                ):
                    upstream_semantic_ledger.validate_ledger(payload, audit)

    def test_rejects_audit_count_drift(self) -> None:
        patch_drift = audit_payload()
        patch_drift["ranges"]["postCheckpoint"]["patchEquivalence"][
            "exactCommitCount"
        ] = 2
        scope_drift = audit_payload()
        scope_drift["ranges"]["postCheckpoint"]["patchEquivalence"][
            "equivalenceScope"
        ] = "unsupported"
        bucket_drift = audit_payload()
        bucket_drift["ranges"]["preCheckpoint"]["primaryPathBuckets"]["core"] = 2
        boundary_drift = audit_payload()
        boundary_drift["ranges"]["preCheckpoint"]["after"] = "f" * 40
        overlap = audit_payload()
        overlap["ranges"]["preCheckpoint"]["commits"][0]["sha"] = COMMITS[0]
        for payload, expected in (
            (patch_drift, "exactCommitCount"),
            (scope_drift, "unsupported algorithm or scope"),
            (bucket_drift, "primaryPathBuckets.core"),
            (boundary_drift, "boundaries"),
            (overlap, "must not overlap"),
        ):
            with self.subTest(expected=expected):
                with self.assertRaisesRegex(
                    upstream_semantic_ledger.LedgerError, expected
                ):
                    upstream_semantic_ledger.validate_audit(payload)

    def test_cli_is_deterministic_and_require_complete_exits_two(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            audit_path = root / "audit.json"
            ledger_path = root / "ledger.json"
            review_path = root / "review.json"
            audit_path.write_text(json.dumps(audit_payload()), encoding="utf-8")
            incomplete = ledger_payload()
            incomplete["rows"] = incomplete["rows"][:1]
            ledger_path.write_text(json.dumps(incomplete), encoding="utf-8")
            review_path.write_text(
                json.dumps(review_payload(incomplete)), encoding="utf-8"
            )
            command = [
                sys.executable,
                str(SCRIPT),
                "validate",
                "--audit",
                str(audit_path),
                "--ledger",
                str(ledger_path),
                "--review",
                str(review_path),
                "--require-complete",
            ]
            first = subprocess.run(command, capture_output=True, text=True, check=False)
            second = subprocess.run(
                command, capture_output=True, text=True, check=False
            )
            self.assertEqual(first.returncode, 3)
            self.assertEqual(
                (first.stdout, first.stderr), (second.stdout, second.stderr)
            )
            for alias in ("--re", "--r"):
                abbreviated = subprocess.run(
                    [*command[:-1], alias],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(
                    (first.returncode, first.stdout, first.stderr),
                    (abbreviated.returncode, abbreviated.stdout, abbreviated.stderr),
                )
            self.assertEqual(json.loads(first.stdout)["range"]["missingCommitCount"], 2)
            self.assertIn("is incomplete", first.stderr)

    def test_committed_upstream_waves_validate(self) -> None:
        upstream_root = SCRIPT.parents[2] / "upstream" / "openai-codex"
        audit_paths = sorted(upstream_root.glob("*/audit.json"))
        self.assertTrue(audit_paths)
        for audit_path in audit_paths:
            wave = audit_path.parent
            audit = upstream_semantic_ledger.validate_audit(
                json.loads(audit_path.read_text(encoding="utf-8"))
            )
            artifact_sets = (
                ("ledger.json", "review.json", "ledger.md"),
                (
                    "pre-checkpoint-ledger.json",
                    "pre-checkpoint-review.json",
                    "pre-checkpoint-ledger.md",
                ),
            )
            for ledger_name, review_name, markdown_name in artifact_sets:
                ledger_path = wave / ledger_name
                review_path = wave / review_name
                markdown_path = wave / markdown_name
                if not ledger_path.exists():
                    self.assertFalse(review_path.exists() or markdown_path.exists())
                    continue
                with self.subTest(wave=wave.name, ledger=ledger_name):
                    self.assertTrue(review_path.exists())
                    self.assertTrue(markdown_path.exists())
                    summary, rows = upstream_semantic_ledger.validate_ledger(
                        json.loads(ledger_path.read_text(encoding="utf-8")), audit
                    )
                    upstream_semantic_ledger.validate_review(
                        json.loads(review_path.read_text(encoding="utf-8")),
                        summary,
                        rows,
                    )
                    self.assertEqual(
                        upstream_semantic_ledger.render(summary, rows),
                        markdown_path.read_text(encoding="utf-8"),
                    )
                    semantic_lines = sum(
                        len((wave / name).read_text(encoding="utf-8").splitlines())
                        for name in (ledger_name, review_name, markdown_name)
                    )
                    self.assertLessEqual(semantic_lines, 800)
