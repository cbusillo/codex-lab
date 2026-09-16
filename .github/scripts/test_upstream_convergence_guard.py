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


APP_SERVER_REGISTRY = "codex-rs/app-server/tests/suite/mod.rs"
V2_REGISTRY = "codex-rs/app-server/tests/suite/v2/mod.rs"

# Owned implementations and proofs that landed after the last guard regeneration.
# Each pairs the path with the contract it is the executable evidence for.
NEW_OWNED_PROOFS = (
    ("codex-rs/app-server/tests/suite/v2/background_review_control.rs", "AGENT-1"),
    ("codex-rs/app-server/tests/suite/v2/project_validation.rs", "VALIDATION-1"),
    ("codex-rs/tui/src/app/thread_routing.rs", "AGENT-1"),
    ("codex-rs/core/tests/suite/session_provenance.rs", "AGENT-1"),
    ("codex-rs/core/src/session/rollout_reconstruction_tests.rs", "HISTORY-1"),
    ("codex-rs/core/tests/suite/turn_context_environments.rs", "HISTORY-1"),
    ("codex-rs/core/src/context/token_budget_context_tests.rs", "CONTEXT-1"),
    ("codex-rs/core/src/context_manager/history_tests.rs", "CONTEXT-1"),
    ("codex-rs/core/src/session_prefix_tests.rs", "CONTEXT-1"),
    ("codex-rs/config/src/hooks_tests.rs", "HOOKS-1"),
    ("codex-rs/hooks/src/engine/mod_tests.rs", "HOOKS-1"),
    ("codex-rs/exec/tests/suite/shared_cli_options.rs", "AUTH-1"),
    # The restored Background Review engine and the durable session state it
    # reads back, plus their dedicated tests.
    ("codex-rs/core/src/tasks/review.rs", "AGENT-1"),
    ("codex-rs/core/src/tasks/review_tests.rs", "AGENT-1"),
    ("codex-rs/core/src/state/session.rs", "AGENT-1"),
    ("codex-rs/core/src/state/session_tests.rs", "AGENT-1"),
    # Restored proofs and implementations the stem convention could not reach.
    ("codex-rs/core/tests/suite/invalid_image_recovery.rs", "CONTEXT-1"),
    ("codex-rs/core/src/browser.rs", "INTEGRATION-1"),
    ("codex-rs/core/src/browser_tests.rs", "INTEGRATION-1"),
    ("codex-rs/core/src/context/world_state/environment_limits.rs", "HISTORY-1"),
    ("codex-rs/tui/src/agent_session_env.rs", "AGENT-1"),
    ("codex-rs/mcp-server/src/approval_response_compat_tests.rs", "SANDBOX-1"),
    ("codex-rs/protocol/src/review_decision_compat.rs", "SANDBOX-1"),
    ("codex-rs/utils/cli/src/approval_mode_cli_arg_tests.rs", "SANDBOX-1"),
)

# Crate-level test binaries. They declare `mod suite;` and carry upstream's
# content, so only their existence is guarded.
PRESENCE_ONLY_REGISTRIES = (
    "codex-rs/core/tests/all.rs",
    "codex-rs/exec/tests/all.rs",
)


def guarded_entry(test: unittest.TestCase, path: str) -> dict[str, object]:
    manifest = {
        entry["path"]: entry for entry in guard.load_manifest(guard.DEFAULT_MANIFEST)
    }
    entry = manifest.get(path)
    test.assertIsNotNone(entry, f"{path} must be a guarded owned path")
    return entry


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
        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", upstream_blob)], []
        )

        self.assertTrue(report["passed"])
        self.assertEqual([], report["violations"])

    def test_fails_when_owned_path_is_absent(self) -> None:
        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", None)], []
        )

        self.assertFalse(report["passed"])
        self.assertEqual(
            [("codex-rs/auto-review/src/lib.rs", guard.ABSENT)],
            [(entry["path"], entry["violation"]) for entry in report["violations"]],
        )

    def test_fails_when_owned_path_reverts_to_upstream_blob(self) -> None:
        upstream_blob = self.write_file("codex-rs/auto-review/src/lib.rs", "upstream\n")
        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", upstream_blob)], []
        )

        self.assertFalse(report["passed"])
        self.assertEqual(guard.REVERTED, report["violations"][0]["violation"])

    def test_passes_when_upstream_lacks_the_path_and_local_keeps_it(self) -> None:
        self.write_file("codex-rs/auto-review/src/lib.rs", "local\n")
        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", None)], []
        )

        self.assertTrue(report["passed"])

    def test_directory_at_owned_path_counts_as_absent(self) -> None:
        (self.root / "codex-rs/auto-review/src/lib.rs").mkdir(parents=True)
        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", None)], []
        )

        self.assertEqual(guard.ABSENT, report["violations"][0]["violation"])

    def test_symbolic_link_at_owned_path_counts_as_absent(self) -> None:
        target = self.root / "target.rs"
        target.write_text("local\n", encoding="utf-8")
        candidate = self.root / "codex-rs" / "auto-review" / "src" / "lib.rs"
        candidate.parent.mkdir(parents=True)
        candidate.symlink_to(target)

        report = self.check(
            [manifest_entry("codex-rs/auto-review/src/lib.rs", None)], []
        )

        self.assertEqual(guard.ABSENT, report["violations"][0]["violation"])
        self.assertIn("symbolic link", report["violations"][0]["detail"])


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


class ManifestValidationTest(GuardFixture):
    def load(self, entries: list[dict[str, object]]) -> list[dict[str, object]]:
        path = self.root / "manifest.json"
        lane_counts: dict[str, int] = {}
        for entry in entries:
            lane = str(entry["lane"])
            lane_counts[lane] = lane_counts.get(lane, 0) + 1
        write_json(
            path,
            {
                "schemaVersion": 1,
                "repository": guard.EXPECTED_REPOSITORY,
                "ownershipBaseline": guard.EXPECTED_OWNERSHIP_BASELINE,
                "policy": {
                    "guardedLanes": list(guard.EXPECTED_GUARDED_LANES),
                    "rule": guard.EXPECTED_MANIFEST_RULE,
                },
                "summary": {
                    "guardedPaths": len(entries),
                    "guardedLaneCounts": dict(sorted(lane_counts.items())),
                },
                "guardedPaths": entries,
            },
        )
        return guard.load_manifest(path)

    def test_rejects_path_escape(self) -> None:
        with self.assertRaises(guard.WaiverError):
            self.load([manifest_entry("../outside", None)])

    def test_rejects_absolute_path(self) -> None:
        with self.assertRaises(guard.WaiverError):
            self.load([manifest_entry("/tmp/outside", None)])

    def test_rejects_duplicate_path(self) -> None:
        entry = manifest_entry("codex-rs/auto-review/src/lib.rs", None)
        with self.assertRaises(guard.WaiverError):
            self.load([entry, entry])

    def test_rejects_changed_ownership_baseline(self) -> None:
        path = self.root / "manifest.json"
        document = {
            "schemaVersion": 1,
            "repository": guard.EXPECTED_REPOSITORY,
            "ownershipBaseline": {
                **guard.EXPECTED_OWNERSHIP_BASELINE,
                "local": "f" * 40,
            },
            "policy": {
                "guardedLanes": list(guard.EXPECTED_GUARDED_LANES),
                "rule": guard.EXPECTED_MANIFEST_RULE,
            },
            "summary": {"guardedPaths": 0, "guardedLaneCounts": {}},
            "guardedPaths": [],
        }
        write_json(path, document)

        with self.assertRaisesRegex(guard.WaiverError, "ownership baseline"):
            guard.load_manifest(path)


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

    def test_owned_app_server_proofs_register_in_guarded_registries(self) -> None:
        """Both app-server registries now carry owned integration proofs."""

        crate_entry = guarded_entry(self, APP_SERVER_REGISTRY)
        v2_entry = guarded_entry(self, V2_REGISTRY)
        waivers = guard.load_waivers(guard.DEFAULT_WAIVERS)

        self.assertEqual("intentionally_owned", crate_entry["lane"])
        self.assertEqual("intentionally_owned", v2_entry["lane"])
        self.assertNotIn(guard.waiver_key(APP_SERVER_REGISTRY, guard.REVERTED), waivers)
        self.assertNotIn(guard.waiver_key(V2_REGISTRY, guard.REVERTED), waivers)

    def test_reverting_the_v2_registry_to_upstream_is_detected(self) -> None:
        entry = guarded_entry(self, V2_REGISTRY)
        contents = (guard.REPO_ROOT / V2_REGISTRY).read_bytes()

        # A recorded upstream blob is what makes reversion detectable at all: a
        # path upstream never had can only be caught by deletion.
        self.assertIsNotNone(entry["upstreamBlob"])
        # The tree is not currently reverted, so the guard reports nothing today.
        self.assertIsNone(guard.evaluate(entry, guard.REPO_ROOT))

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            target = root / V2_REGISTRY
            target.parent.mkdir(parents=True)
            target.write_bytes(contents)
            reverted = {**entry, "upstreamBlob": guard.blob_id(contents)}

            self.assertEqual(guard.REVERTED, guard.evaluate(reverted, root)[0])

    def test_deleting_any_new_owned_proof_is_detected(self) -> None:
        """Each proof below is the only executable evidence for its contract."""

        with tempfile.TemporaryDirectory() as temp_dir:
            empty_root = Path(temp_dir)
            for path, contract in NEW_OWNED_PROOFS:
                with self.subTest(path=path):
                    entry = guarded_entry(self, path)
                    self.assertEqual("intentionally_owned", entry["lane"])
                    self.assertIn(contract, entry["contracts"])
                    self.assertTrue((guard.REPO_ROOT / path).is_file())
                    self.assertEqual(guard.ABSENT, guard.evaluate(entry, empty_root)[0])

    def test_new_owned_proofs_are_not_waived(self) -> None:
        waived = {path for path, _ in guard.load_waivers(guard.DEFAULT_WAIVERS)}

        for path, _ in NEW_OWNED_PROOFS:
            with self.subTest(path=path):
                self.assertNotIn(path, waived)

    def test_deleting_a_crate_test_binary_is_detected(self) -> None:
        """Losing `all.rs` stops every owned suite in that crate from running.

        These files never diverged from upstream, so the ordinary "reverted to
        upstream" signal cannot apply to them. They are guarded for absence
        alone, which is the failure mode that actually silences the proofs.
        """

        with tempfile.TemporaryDirectory() as temp_dir:
            empty_root = Path(temp_dir)
            for path in PRESENCE_ONLY_REGISTRIES:
                with self.subTest(path=path):
                    entry = guarded_entry(self, path)
                    self.assertEqual("intentionally_owned", entry["lane"])
                    self.assertEqual(guard.PRESENCE_ONLY_GUARD, entry["guard"])
                    self.assertEqual(guard.ABSENT, guard.evaluate(entry, empty_root)[0])

    def test_presence_only_registry_carrying_upstream_content_is_intact(self) -> None:
        # These files hold upstream's content by design, so the reversion check
        # would fire on the intact tree if it were applied to them.
        for path in PRESENCE_ONLY_REGISTRIES:
            with self.subTest(path=path):
                entry = guarded_entry(self, path)
                contents = (guard.REPO_ROOT / path).read_bytes()

                self.assertEqual(guard.blob_id(contents), entry["upstreamBlob"])
                self.assertIsNone(guard.evaluate(entry, guard.REPO_ROOT))

    def test_every_waiver_names_a_guarded_path(self) -> None:
        guarded = {
            entry["path"] for entry in guard.load_manifest(guard.DEFAULT_MANIFEST)
        }
        waived = {path for path, _ in guard.load_waivers(guard.DEFAULT_WAIVERS)}

        self.assertEqual(set(), waived - guarded)


if __name__ == "__main__":
    unittest.main()
