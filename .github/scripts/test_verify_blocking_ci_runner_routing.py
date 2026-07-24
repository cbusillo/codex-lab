import tempfile
import unittest
from pathlib import Path

import verify_blocking_ci_runner_routing as routing


class VerifyBlockingCiRunnerRoutingTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.workflows_dir = Path(self.temp_dir.name) / ".github/workflows"
        self.workflows_dir.mkdir(parents=True)

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def write_workflow(self, name: str, contents: str) -> None:
        (self.workflows_dir / name).write_text(contents)

    def test_accepts_hosted_runner_workflow_graph(self) -> None:
        self.write_workflow(
            "blocking-ci.yml",
            "jobs:\n  child:\n    uses: ./.github/workflows/child.yml\n",
        )
        self.write_workflow(
            "child.yml",
            "jobs:\n  linux:\n    runs-on: ubuntu-24.04\n"
            "  macos:\n    runs-on: macos-26\n"
            "  windows:\n    runs-on: windows-latest\n",
        )

        self.assertEqual(routing.find_violations(self.workflows_dir), [])

    def test_rejects_unsupported_runner_selectors(self) -> None:
        self.write_workflow(
            "blocking-ci.yml",
            "jobs:\n"
            "  grouped:\n"
            "    runs-on:\n"
            "      group: codex-runners\n"
            "      labels: ${{ github.event.repository.name }}-linux-x64\n"
            "  local:\n"
            "    runs-on: [self-hosted, Linux, X64, codex-lab-linux]\n"
            "  macos:\n"
            "    runs-on: macos-15-xlarge\n"
            "  windows:\n"
            "    runs-on: windows-x64\n",
        )

        reasons = {
            violation.reason
            for violation in routing.find_violations(self.workflows_dir)
        }
        self.assertEqual(
            reasons,
            {
                "runner group selector",
                "repository-derived runner selector",
                "persistent self-hosted runner",
                "billable macOS large runner",
                "unsupported platform runner alias",
            },
        )

    def test_checks_nested_reusable_workflows(self) -> None:
        self.write_workflow(
            "blocking-ci.yml",
            "jobs:\n  first:\n    uses: ./.github/workflows/first.yml\n",
        )
        self.write_workflow(
            "first.yml",
            "jobs:\n  second:\n    uses: ./.github/workflows/second.yml\n",
        )
        self.write_workflow(
            "second.yml",
            "jobs:\n  macos:\n    runs-on: macos-15-large\n",
        )

        violations = routing.find_violations(self.workflows_dir)
        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].path.name, "second.yml")

    def test_ignores_self_hosted_runners_outside_blocking_graph(self) -> None:
        self.write_workflow(
            "blocking-ci.yml",
            "jobs:\n  hosted:\n    runs-on: ubuntu-24.04\n",
        )
        self.write_workflow(
            "postmerge-ci.yml",
            "jobs:\n"
            "  trusted:\n"
            "    runs-on: [self-hosted, codex-lab-linux]\n",
        )

        self.assertEqual(routing.find_violations(self.workflows_dir), [])

    def test_reports_missing_reusable_workflow(self) -> None:
        self.write_workflow(
            "blocking-ci.yml",
            "jobs:\n  missing:\n    uses: ./.github/workflows/missing.yml\n",
        )

        violations = routing.find_violations(self.workflows_dir)
        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].reason, "referenced workflow does not exist")


if __name__ == "__main__":
    unittest.main()
