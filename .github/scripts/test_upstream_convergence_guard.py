import json
import tempfile
import unittest
from pathlib import Path

import upstream_convergence_guard as guard


def write_json(path: Path, value: dict[str, object]) -> None:
    path.write_text(json.dumps(value), encoding="utf-8")


def manifest_entry(path: str, upstream_blob: str | None) -> dict[str, object]:
    return {
        "path": path,
        "lane": "intentionally_owned",
        "contracts": ["AGENT-1"],
        "reason": "Every Code orchestration and review behavior",
        "baselineBlob": "0" * 40,
        "upstreamBlob": upstream_blob,
    }


def waiver(path: str, violation: str, **overrides: object) -> dict[str, object]:
    entry = {
        "path": path,
        "violation": violation,
        "disposition": "pending_restore",
        "issue": 428,
        "reason": "lost at the anchor merge",
    }
    entry.update(overrides)
    return entry


class GuardFixture(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.root = Path(self._tmp.name)

    def write_file(self, path: str, contents: str) -> str:
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        data = contents.encode("utf-8")
        target.write_bytes(data)
        return guard.blob_id(data)

    def check(
        self,
        manifest: list[dict[str, object]],
        waivers: list[dict[str, object]],
    ) -> dict[str, object]:
        waiver_path = self.root / "waivers.json"
        write_json(waiver_path, {"schemaVersion": 1, "waivers": waivers})
        return guard.check(manifest, guard.load_waivers(waiver_path), self.root)


class BlobIdTest(unittest.TestCase):
    def test_matches_git_blob_hash(self) -> None:
        # `printf 'owned\n' | git hash-object --stdin`
        self.assertEqual(
            "e6640e8379a3df4fa8fec2a4e6045ca6e7bbbd5d",
            guard.blob_id(b"owned\n"),
        )


class GuardViolationTest(GuardFixture):
    def test_passes_when_owned_path_keeps_local_content(self) -> None:
        upstream_blob = guard.blob_id(b"upstream\n")
        self.write_file("codex-rs/auto-review/src/lib.rs", "local\n")
        report = self.check([manifest_entry("codex-rs/auto-review/src/lib.rs", upstream_blob)], [])

        self.assertTrue(report["passed"])
        self.assertEqual([], report["violations"])

    def test_fails_when_owned_path_is_absent(self) -> None:
        report = self.check([manifest_entry("codex-rs/auto-review/src/lib.rs", None)], [])

        self.assertFalse(report["passed"])
        self.assertEqual(
            [("codex-rs/auto-review/src/lib.rs", guard.ABSENT)],
            [(entry["path"], entry["violation"]) for entry in report["violations"]],
        )

    def test_fails_when_owned_path_reverts_to_upstream_blob(self) -> None:
        upstream_blob = self.write_file("codex-rs/auto-review/src/lib.rs", "upstream\n")
        report = self.check([manifest_entry("codex-rs/auto-review/src/lib.rs", upstream_blob)], [])

        self.assertFalse(report["passed"])
        self.assertEqual(guard.REVERTED, report["violations"][0]["violation"])

    def test_passes_when_upstream_lacks_the_path_and_local_keeps_it(self) -> None:
        self.write_file("codex-rs/auto-review/src/lib.rs", "local\n")
        report = self.check([manifest_entry("codex-rs/auto-review/src/lib.rs", None)], [])

        self.assertTrue(report["passed"])

    def test_directory_at_owned_path_counts_as_absent(self) -> None:
        (self.root / "codex-rs/auto-review/src/lib.rs").mkdir(parents=True)
        report = self.check([manifest_entry("codex-rs/auto-review/src/lib.rs", None)], [])

        self.assertEqual(guard.ABSENT, report["violations"][0]["violation"])


class GuardWaiverTest(GuardFixture):
    def test_waiver_clears_the_matching_violation(self) -> None:
        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", None)],
            [waiver("codex-rs/auto-review/src/lib.rs", guard.ABSENT)],
        )

        self.assertTrue(report["passed"])
        self.assertEqual(1, len(report["waived"]))
        self.assertEqual(428, report["waived"][0]["issue"])

    def test_waiver_does_not_cover_a_different_violation(self) -> None:
        upstream_blob = self.write_file("codex-rs/auto-review/src/lib.rs", "upstream\n")
        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", upstream_blob)],
            [waiver("codex-rs/auto-review/src/lib.rs", guard.ABSENT)],
        )

        self.assertFalse(report["passed"])
        self.assertEqual(guard.REVERTED, report["violations"][0]["violation"])

    def test_stale_waiver_fails_after_the_path_is_restored(self) -> None:
        self.write_file("codex-rs/auto-review/src/lib.rs", "local\n")
        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", None)],
            [waiver("codex-rs/auto-review/src/lib.rs", guard.ABSENT)],
        )

        self.assertFalse(report["passed"])
        self.assertEqual([], report["violations"])
        self.assertEqual(
            ["codex-rs/auto-review/src/lib.rs"],
            [entry["path"] for entry in report["staleWaivers"]],
        )

    def test_upstream_deletion_of_an_unowned_path_is_not_guarded(self) -> None:
        # Green and amber lanes never enter the manifest, so a legitimate
        # upstream deletion there cannot fail the guard.
        report = self.check([], [])

        self.assertTrue(report["passed"])
        self.assertEqual(0, report["guardedPaths"])

    def test_adopted_upstream_deletion_is_an_explicit_disposition(self) -> None:
        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", None)],
            [
                waiver(
                    "codex-rs/auto-review/src/lib.rs",
                    guard.ABSENT,
                    disposition="upstream_deletion_adopted",
                )
            ],
        )

        self.assertTrue(report["passed"])
        self.assertEqual(
            "upstream_deletion_adopted", report["waived"][0]["disposition"]
        )


class WaiverLedgerValidationTest(GuardFixture):
    def load(self, document: dict[str, object]) -> dict[tuple[str, str], object]:
        path = self.root / "waivers.json"
        write_json(path, document)
        return guard.load_waivers(path)

    def test_rejects_unknown_schema_version(self) -> None:
        with self.assertRaises(guard.WaiverError):
            self.load({"schemaVersion": 99, "waivers": []})

    def test_rejects_waiver_without_a_reason(self) -> None:
        entry = waiver("a", guard.ABSENT)
        entry["reason"] = "   "
        with self.assertRaises(guard.WaiverError):
            self.load({"schemaVersion": 1, "waivers": [entry]})

    def test_rejects_waiver_without_a_deciding_issue(self) -> None:
        entry = waiver("a", guard.ABSENT)
        del entry["issue"]
        with self.assertRaises(guard.WaiverError):
            self.load({"schemaVersion": 1, "waivers": [entry]})

    def test_rejects_unknown_disposition(self) -> None:
        with self.assertRaises(guard.WaiverError):
            self.load(
                {
                    "schemaVersion": 1,
                    "waivers": [waiver("a", guard.ABSENT, disposition="because")],
                }
            )

    def test_rejects_unknown_violation(self) -> None:
        with self.assertRaises(guard.WaiverError):
            self.load({"schemaVersion": 1, "waivers": [waiver("a", "whatever")]})

    def test_rejects_duplicate_waivers(self) -> None:
        with self.assertRaises(guard.WaiverError):
            self.load(
                {
                    "schemaVersion": 1,
                    "waivers": [waiver("a", guard.ABSENT), waiver("a", guard.ABSENT)],
                }
            )


class CheckedInLedgerTest(unittest.TestCase):
    def test_repository_guard_manifest_and_waivers_agree(self) -> None:
        manifest = guard.load_manifest(guard.DEFAULT_MANIFEST)
        waivers = guard.load_waivers(guard.DEFAULT_WAIVERS)
        report = guard.check(manifest, waivers, guard.REPO_ROOT)

        self.assertEqual([], report["violations"])
        self.assertEqual([], report["staleWaivers"])

    def test_owned_external_agent_proofs_are_guarded_and_unwaived(self) -> None:
        """A refresh must not be able to delete the AGENT-1 preflight proofs.

        These paths carry the only executable evidence for explicit
        external-agent preflight, so an unwaived `absent` violation is the
        intended failure when a snapshot merge drops them.
        """

        manifest = {
            entry["path"]: entry
            for entry in guard.load_manifest(guard.DEFAULT_MANIFEST)
        }
        waived = {path for path, _ in guard.load_waivers(guard.DEFAULT_WAIVERS)}

        for path in (
            "codex-rs/core/tests/suite/external_agent_preflight.rs",
            "codex-rs/core/src/agent/external_preflight.rs",
            "codex-rs/core/src/agent/external_preflight_tests.rs",
        ):
            with self.subTest(path=path):
                entry = manifest.get(path)
                self.assertIsNotNone(entry, f"{path} must be a guarded owned path")
                self.assertEqual("intentionally_owned", entry["lane"])
                self.assertIn("AGENT-1", entry["contracts"])
                self.assertNotIn(path, waived)

    def test_restored_owned_proofs_are_guarded_and_unwaived(self) -> None:
        """The restored implementations and proofs cannot silently disappear again.

        Each path below is either an owned implementation or the executable proof
        that pins it. An unwaived guard entry is what turns a future refresh that
        drops one into a CI failure instead of silent evidence loss.
        """

        manifest = {
            entry["path"]: entry
            for entry in guard.load_manifest(guard.DEFAULT_MANIFEST)
        }
        waived = {path for path, _ in guard.load_waivers(guard.DEFAULT_WAIVERS)}

        for path, contract in (
            # Project Validation
            ("codex-rs/core/src/session/project_validation.rs", "VALIDATION-1"),
            ("codex-rs/core/src/session/validation_provider.rs", "VALIDATION-1"),
            ("codex-rs/core/tests/suite/project_validation.rs", "VALIDATION-1"),
            ("codex-rs/exec/tests/suite/project_validation_event.rs", "VALIDATION-1"),
            # Background Review
            ("codex-rs/core/src/session/background_auto_review.rs", "AGENT-1"),
            ("codex-rs/core/tests/suite/background_review.rs", "AGENT-1"),
            # Code Bridge and browser model handlers plus their proofs
            ("codex-rs/core/src/tools/handlers/code_bridge.rs", "INTEGRATION-1"),
            ("codex-rs/core/src/tools/handlers/browser.rs", "INTEGRATION-1"),
            ("codex-rs/core/tests/suite/tools.rs", "INTEGRATION-1"),
            ("codex-rs/app-server/tests/suite/v2/code_bridge.rs", "INTEGRATION-1"),
            ("codex-rs/app-server/tests/suite/v2/remote_control.rs", "INTEGRATION-1"),
            # External-agent preflight and routing
            ("codex-rs/core/tests/suite/external_agent_preflight.rs", "AGENT-1"),
            ("codex-rs/core/src/agent/provider_routing.rs", "AGENT-1"),
            # Registration points: reverting these unregisters owned suites
            # without deleting a single proof file.
            ("codex-rs/core/tests/suite/mod.rs", "INTEGRATION-1"),
            ("codex-rs/exec/tests/suite/mod.rs", "VALIDATION-1"),
        ):
            with self.subTest(path=path):
                entry = manifest.get(path)
                self.assertIsNotNone(entry, f"{path} must be a guarded owned path")
                self.assertEqual("intentionally_owned", entry["lane"])
                self.assertIn(contract, entry["contracts"])
                self.assertNotIn(path, waived)

    def test_manifest_records_why_each_path_is_guarded(self) -> None:
        # Owned work created after the pinned ownership baseline is only guarded
        # by the current-tree source, so a manifest that lost it would quietly
        # stop protecting every restored proof.
        entries = guard.load_manifest(guard.DEFAULT_MANIFEST)
        sources = {entry.get("source") for entry in entries}

        self.assertEqual({"ownership_baseline", "current_tree"}, sources)

    def test_every_waiver_names_a_guarded_path(self) -> None:
        guarded = {entry["path"] for entry in guard.load_manifest(guard.DEFAULT_MANIFEST)}
        waived = {path for path, _ in guard.load_waivers(guard.DEFAULT_WAIVERS)}

        self.assertEqual(set(), waived - guarded)


if __name__ == "__main__":
    unittest.main()
