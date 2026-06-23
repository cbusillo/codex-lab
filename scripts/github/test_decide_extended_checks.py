#!/usr/bin/env python3
import unittest
from pathlib import Path
from unittest import mock

import decide_extended_checks


class DecideExtendedChecksTest(unittest.TestCase):
    def test_codex_rs_change_requires_both_extended_checks(self) -> None:
        checks = decide_extended_checks.load_checks(
            decide_extended_checks.DEFAULT_CONFIG
        )

        decisions = decide_extended_checks.decide_checks(
            checks,
            ["codex-rs/tui/src/app.rs"],
            head_repo="cbusillo/codex-lab",
            base_repo="cbusillo/codex-lab",
        )

        self.assertEqual(
            decision_summary(decisions),
            {
                "codex-lab-app": {
                    "required": True,
                    "available": True,
                    "matched_paths": ("codex-rs/tui/src/app.rs",),
                    "skip_reason": None,
                    "unavailable_reason": None,
                },
                "exec-harness": {
                    "required": True,
                    "available": True,
                    "matched_paths": ("codex-rs/tui/src/app.rs",),
                    "skip_reason": None,
                    "unavailable_reason": None,
                },
            },
        )

    def test_unmatched_docs_change_skips_extended_checks(self) -> None:
        checks = decide_extended_checks.load_checks(
            decide_extended_checks.DEFAULT_CONFIG
        )

        decisions = decide_extended_checks.decide_checks(
            checks,
            ["README.md"],
            head_repo="cbusillo/codex-lab",
            base_repo="cbusillo/codex-lab",
        )

        self.assertEqual(
            decision_summary(decisions),
            {
                "codex-lab-app": {
                    "required": False,
                    "available": True,
                    "matched_paths": (),
                    "skip_reason": None,
                    "unavailable_reason": None,
                },
                "exec-harness": {
                    "required": False,
                    "available": True,
                    "matched_paths": (),
                    "skip_reason": None,
                    "unavailable_reason": None,
                },
            },
        )

    def test_fork_pull_request_marks_self_hosted_check_manual(self) -> None:
        checks = decide_extended_checks.load_checks(
            decide_extended_checks.DEFAULT_CONFIG
        )

        decisions = decide_extended_checks.decide_checks(
            checks,
            ["tools/codex-exec-harness/harness.py"],
            head_repo="contributor/codex-lab",
            base_repo="cbusillo/codex-lab",
        )

        self.assertEqual(
            decision_summary(decisions)["exec-harness"],
            {
                "required": True,
                "available": False,
                "matched_paths": ("tools/codex-exec-harness/harness.py",),
                "skip_reason": None,
                "unavailable_reason": (
                    "exec-harness requires self-hosted runners and cannot run "
                    "automatically for fork pull requests"
                ),
            },
        )

    def test_push_event_skips_pull_request_only_workflows(self) -> None:
        checks = decide_extended_checks.load_checks(
            decide_extended_checks.DEFAULT_CONFIG
        )

        decisions = decide_extended_checks.decide_checks(
            checks,
            ["codex-rs/tui/src/app.rs"],
            head_repo="cbusillo/codex-lab",
            base_repo="cbusillo/codex-lab",
            event_name="push",
        )

        self.assertEqual(
            decision_summary(decisions)["codex-lab-app"],
            {
                "required": False,
                "available": True,
                "matched_paths": (),
                "skip_reason": (
                    ".github/workflows/codex-lab-app.yml runs automatically "
                    "for pull_request changes; push does not trigger it."
                ),
                "unavailable_reason": None,
            },
        )

    def test_markdown_explains_required_and_skipped_checks(self) -> None:
        checks = decide_extended_checks.load_checks(
            decide_extended_checks.DEFAULT_CONFIG
        )
        decisions = decide_extended_checks.decide_checks(
            checks,
            ["scripts/codex_lab_package/layout.py"],
            head_repo="cbusillo/codex-lab",
            base_repo="cbusillo/codex-lab",
        )

        markdown = decide_extended_checks.markdown_for(
            decisions,
            ["scripts/codex_lab_package/layout.py"],
        )

        self.assertIn("`codex-lab-app` | required", markdown)
        self.assertIn("`exec-harness` | skipped", markdown)
        self.assertIn("`scripts/codex_lab_package/layout.py`", markdown)

    def test_git_changed_files_uses_merge_base_for_all_zero_base(self) -> None:
        completed = mock.Mock(stdout="README.md\n")
        with (
            mock.patch.object(
                decide_extended_checks,
                "default_branch_merge_base",
                return_value="base-sha",
            ),
            mock.patch.object(
                decide_extended_checks.subprocess,
                "run",
                return_value=completed,
            ) as run,
        ):
            changed_files = decide_extended_checks.git_changed_files("0" * 40, "HEAD")

        self.assertEqual(changed_files, ["README.md"])
        self.assertEqual(
            run.call_args.args[0],
            ["git", "diff", "--name-only", "base-sha...HEAD"],
        )

    def test_git_changed_files_falls_back_to_diff_tree_for_all_zero_base(self) -> None:
        completed = mock.Mock(stdout="README.md\n")
        with (
            mock.patch.object(
                decide_extended_checks,
                "default_branch_merge_base",
                return_value=None,
            ),
            mock.patch.object(
                decide_extended_checks.subprocess,
                "run",
                return_value=completed,
            ) as run,
        ):
            changed_files = decide_extended_checks.git_changed_files("0" * 40, "HEAD")

        self.assertEqual(changed_files, ["README.md"])
        self.assertEqual(
            run.call_args.args[0],
            [
                "git",
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-only",
                "-r",
                "HEAD",
            ],
        )

    def test_validate_config_accepts_repo_config(self) -> None:
        checks = decide_extended_checks.load_checks(
            decide_extended_checks.DEFAULT_CONFIG
        )

        decide_extended_checks.validate_config(
            checks,
            decide_extended_checks.DEFAULT_CONFIG,
        )

    def test_validate_config_catches_uncovered_workflow_script(self) -> None:
        check = decide_extended_checks.ExtendedCheck(
            name="example",
            description="",
            workflow=".github/workflows/example.yml",
            same_repo_only=False,
            patterns=(
                ".github/extended-checks.json",
                ".github/workflows/example.yml",
                "scripts/github/decide_extended_checks.py",
                "scripts/github/test_decide_extended_checks.py",
            ),
        )
        workflow_text = """
on:
  pull_request:
    paths:
      - ".github/extended-checks.json"
      - ".github/workflows/example.yml"
      - "scripts/github/decide_extended_checks.py"
      - "scripts/github/test_decide_extended_checks.py"
jobs:
  test:
    steps:
      - run: scripts/example.sh
"""

        with (
            mock.patch.object(Path, "is_file", return_value=True),
            mock.patch.object(
                Path,
                "read_text",
                return_value=workflow_text,
            ),
            mock.patch.object(
                decide_extended_checks,
                "repo_files",
                return_value=[
                    ".github/extended-checks.json",
                    ".github/workflows/example.yml",
                    "scripts/github/decide_extended_checks.py",
                    "scripts/github/test_decide_extended_checks.py",
                    "scripts/example.sh",
                ],
            ),
        ):
            with self.assertRaisesRegex(
                ValueError,
                "workflow references scripts/example.sh",
            ):
                decide_extended_checks.validate_config(
                    [check],
                    decide_extended_checks.DEFAULT_CONFIG,
                )

    def test_validate_config_catches_workflow_path_drift(self) -> None:
        check = decide_extended_checks.ExtendedCheck(
            name="example",
            description="",
            workflow=".github/workflows/example.yml",
            same_repo_only=False,
            patterns=(
                ".github/extended-checks.json",
                ".github/workflows/example.yml",
                "scripts/github/decide_extended_checks.py",
                "scripts/github/test_decide_extended_checks.py",
                "scripts/example.sh",
            ),
        )
        workflow_text = """
on:
  pull_request:
    paths:
      - ".github/extended-checks.json"
      - ".github/workflows/example.yml"
      - "scripts/github/decide_extended_checks.py"
      - "scripts/github/test_decide_extended_checks.py"
jobs:
  test:
    steps:
      - run: scripts/example.sh
"""

        with (
            mock.patch.object(Path, "is_file", return_value=True),
            mock.patch.object(Path, "read_text", return_value=workflow_text),
            mock.patch.object(
                decide_extended_checks,
                "repo_files",
                return_value=[
                    ".github/extended-checks.json",
                    ".github/workflows/example.yml",
                    "scripts/github/decide_extended_checks.py",
                    "scripts/github/test_decide_extended_checks.py",
                    "scripts/example.sh",
                ],
            ),
        ):
            with self.assertRaisesRegex(
                ValueError,
                "workflow pull_request.paths differ",
            ):
                decide_extended_checks.validate_config(
                    [check],
                    decide_extended_checks.DEFAULT_CONFIG,
                )


def decision_summary(
    decisions: list[decide_extended_checks.CheckDecision],
) -> dict[str, dict[str, object]]:
    return {
        decision.name: {
            "required": decision.required,
            "available": decision.available,
            "matched_paths": decision.matched_paths,
            "skip_reason": decision.skip_reason,
            "unavailable_reason": decision.unavailable_reason,
        }
        for decision in decisions
    }


if __name__ == "__main__":
    unittest.main()
