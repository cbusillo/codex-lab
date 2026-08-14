"""Coverage for the repo-checks Python test registration verifier."""

import tempfile
import unittest
from pathlib import Path

from verify_repo_checks_test_registration import DEFAULT_WORKFLOW
from verify_repo_checks_test_registration import REPO_ROOT
from verify_repo_checks_test_registration import discovery_registrations
from verify_repo_checks_test_registration import main
from verify_repo_checks_test_registration import unregistered_tests


WRAPPED_WORKFLOW = """
      - name: Wrapped discovery
        run: |
          python3 -m unittest discover -s .github/scripts \\
            -p 'test_upstream_convergence_*.py'
"""


class DiscoveryRegistrationsTest(unittest.TestCase):
    def test_wrapped_invocation_is_read_like_a_single_line(self) -> None:
        self.assertEqual(
            discovery_registrations(WRAPPED_WORKFLOW),
            {".github/scripts": ["test_upstream_convergence_*.py"]},
        )

    def test_repeated_directory_collects_every_pattern(self) -> None:
        workflow = (
            "run: python3 -m unittest discover -s pkg -p 'test_a.py'\n"
            "run: python3 -m unittest discover -s pkg -p \"test_b.py\"\n"
        )

        self.assertEqual(
            discovery_registrations(workflow), {"pkg": ["test_a.py", "test_b.py"]}
        )


class UnregisteredTestsTest(unittest.TestCase):
    def test_file_outside_every_pattern_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            (root / "pkg").mkdir()
            (root / "pkg" / "test_registered.py").write_text("")
            (root / "pkg" / "test_forgotten.py").write_text("")

            problems = unregistered_tests({"pkg": ["test_registered.py"]}, root)

            self.assertEqual(
                [path for path, _ in problems], ["pkg/test_forgotten.py"]
            )

    def test_directory_wide_pattern_covers_new_files(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            (root / "pkg").mkdir()
            (root / "pkg" / "test_anything.py").write_text("")

            self.assertEqual(unregistered_tests({"pkg": ["test_*.py"]}, root), [])

    def test_nested_test_needs_a_package_marker(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            nested = root / "pkg" / "nested"
            nested.mkdir(parents=True)
            (nested / "test_nested.py").write_text("")

            problems = unregistered_tests({"pkg": ["test_*.py"]}, root)

            self.assertEqual(
                [path for path, _ in problems], ["pkg/nested/test_nested.py"]
            )

            (nested / "__init__.py").write_text("")
            self.assertEqual(unregistered_tests({"pkg": ["test_*.py"]}, root), [])

    def test_explicit_nested_discovery_does_not_need_a_package_marker(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            nested = root / "pkg" / "nested-tools"
            nested.mkdir(parents=True)
            (nested / "test_nested.py").write_text("")

            self.assertEqual(
                unregistered_tests(
                    {
                        "pkg": ["test_*.py"],
                        "pkg/nested-tools": ["test_*.py"],
                    },
                    root,
                ),
                [],
            )

    def test_missing_discovery_directory_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            problems = unregistered_tests({"gone": ["test_*.py"]}, Path(temp_dir))

            self.assertEqual(problems, [("gone", "discovery directory does not exist")])


class MainTest(unittest.TestCase):
    def test_repo_checks_registers_every_github_script_test(self) -> None:
        self.assertEqual(main([]), 0)

    def test_workflow_without_discovery_steps_fails(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            workflow = Path(temp_dir) / "repo-checks.yml"
            workflow.write_text("jobs: {}\n")

            self.assertEqual(main(["--workflow", str(workflow)]), 2)

    def test_unreadable_workflow_fails(self) -> None:
        missing = Path(tempfile.gettempdir()) / "definitely-missing-workflow.yml"

        self.assertEqual(main(["--workflow", str(missing)]), 2)


class RepoCheckWiringTest(unittest.TestCase):
    """These are the files that were sitting in the tree without ever running."""

    PREVIOUSLY_UNREGISTERED = (
        "test_macos_signing_entitlements.py",
        "test_run_bazel_with_buildbuddy.py",
        "test_rusty_v8_bazel.py",
        "test_v8_canary_changes.py",
    )

    def test_previously_unregistered_suites_now_run(self) -> None:
        registrations = discovery_registrations(
            DEFAULT_WORKFLOW.read_text(encoding="utf-8")
        )
        scripts = REPO_ROOT / ".github" / "scripts"

        for name in self.PREVIOUSLY_UNREGISTERED:
            with self.subTest(name=name):
                self.assertTrue((scripts / name).is_file())
                self.assertEqual(unregistered_tests(registrations, REPO_ROOT), [])


if __name__ == "__main__":
    unittest.main()
