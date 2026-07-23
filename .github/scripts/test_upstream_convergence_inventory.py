import unittest

import upstream_convergence_inventory as inventory


class UpstreamConvergenceInventoryTest(unittest.TestCase):
    def test_parses_content_conflict(self) -> None:
        self.assertEqual(
            inventory.parse_conflict_message(
                "CONFLICT (content): Merge conflict in codex-rs/core/src/lib.rs"
            ),
            ("content", "codex-rs/core/src/lib.rs"),
        )

    def test_parses_modify_delete_conflict(self) -> None:
        self.assertEqual(
            inventory.parse_conflict_message(
                "CONFLICT (modify/delete): path/file.rs deleted in local and modified in upstream. Version upstream of path/file.rs left in tree."
            ),
            ("modify/delete", "path/file.rs"),
        )

    def test_defaults_unowned_path_to_upstream(self) -> None:
        self.assertEqual(
            inventory.classify("codex-rs/core/src/lib.rs", "content"),
            {
                "path": "codex-rs/core/src/lib.rs",
                "conflictType": "content",
                "lane": "green_bulk_adopt",
                "contracts": [],
                "reason": "upstream-owned surface with no named local contract",
            },
        )

    def test_red_lane_wins_when_contracts_overlap(self) -> None:
        classified = inventory.classify("codex-rs/cli/src/login.rs", "content")
        self.assertEqual(classified["lane"], "red_manual_review")
        self.assertEqual(classified["contracts"], ["IDENTITY-1"])

    def test_protocol_path_is_contract_adapted(self) -> None:
        classified = inventory.classify(
            "codex-rs/app-server-protocol/src/protocol/v2/account.rs",
            "content",
        )
        self.assertEqual(classified["lane"], "amber_contract_adapt")
        self.assertEqual(classified["contracts"], ["PROTOCOL-1"])

    def test_agent_path_is_intentionally_owned(self) -> None:
        classified = inventory.classify(
            "codex-rs/core/src/agent/control.rs",
            "content",
        )
        self.assertEqual(classified["lane"], "intentionally_owned")
        self.assertEqual(classified["contracts"], ["AGENT-1"])

if __name__ == "__main__":
    unittest.main()
