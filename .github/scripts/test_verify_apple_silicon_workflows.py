import tempfile
import unittest
from pathlib import Path

from verify_apple_silicon_workflows import active_workflows
from verify_apple_silicon_workflows import repository_violations
from verify_apple_silicon_workflows import selector_violations


ROOT = Path(__file__).resolve().parents[2]


class AppleSiliconWorkflowPolicyTest(unittest.TestCase):
    def test_repository_workflow_graph_is_apple_silicon_only(self) -> None:
        self.assertEqual(repository_violations(ROOT), [])

    def test_active_graph_follows_reusable_workflows(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            workflows = Path(temp_dir)
            (workflows / "entry.yml").write_text(
                "on:\n  workflow_dispatch:\njobs:\n  call:\n"
                "    uses: ./.github/workflows/reusable.yml\n"
            )
            (workflows / "reusable.yml").write_text(
                "on:\n  workflow_call:\njobs:\n  test:\n    runs-on: ubuntu-latest\n"
            )

            active, violations = active_workflows(
                workflows,
                out_of_scope_entrypoints=frozenset(),
            )

        self.assertEqual({path.name for path in active}, {"entry.yml", "reusable.yml"})
        self.assertEqual(violations, [])

    def test_linux_runner_is_rejected(self) -> None:
        path = Path("workflow.yml")
        violations = selector_violations(
            path,
            "jobs:\n  test:\n    runs-on: ubuntu-latest\n",
        )

        self.assertEqual(len(violations), 1)
        self.assertIn("non-Apple-Silicon runner", violations[0].reason)

    def test_non_apple_target_is_rejected(self) -> None:
        path = Path("workflow.yml")
        violations = selector_violations(
            path,
            "jobs:\n  test:\n    runs-on: macos-26\n"
            "    strategy:\n      matrix:\n        include:\n"
            "          - target: x86_64-apple-darwin\n",
        )

        self.assertEqual(len(violations), 1)
        self.assertIn("non-Apple-Silicon target", violations[0].reason)

    def test_linux_container_action_is_rejected(self) -> None:
        path = Path("workflow.yml")
        violations = selector_violations(
            path,
            "jobs:\n  test:\n    runs-on: macos-26\n    steps:\n"
            "      - uses: codespell-project/actions-codespell@pinned\n",
        )

        self.assertEqual(len(violations), 1)
        self.assertIn("Linux container action", violations[0].reason)

    def test_multiline_self_hosted_apple_labels_are_allowed(self) -> None:
        path = Path("workflow.yml")
        violations = selector_violations(
            path,
            "jobs:\n  test:\n    runs-on:\n"
            "      - self-hosted\n      - macOS\n      - ARM64\n",
        )

        self.assertEqual(violations, [])


if __name__ == "__main__":
    unittest.main()
