"""Exercise the actual workflow shell guards without starting any builds."""

import itertools
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]


def shell_block(contents: str, marker: str) -> str:
    tail = contents.split(marker, 1)[1].split("run: |\n", 1)[1]
    lines = tail.splitlines()
    indent = len(lines[0]) - len(lines[0].lstrip())
    selected = []
    for line in lines:
        if line.strip() and len(line) - len(line.lstrip()) < indent:
            break
        selected.append(line)
    return textwrap.dedent("\n".join(selected))


class ReleaseRustRoutingTests(unittest.TestCase):
    def run_shell(
        self, script: str, *, executable: str = "bash", **overrides: str
    ) -> tuple[int, str, str, str, str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output, environment, path_file = (
                root / name for name in ("output", "env", "path")
            )
            for path in (output, environment, path_file):
                path.touch()
            env = {
                "PATH": os.environ["PATH"],
                "GITHUB_ACTIONS": "true",
                "GITHUB_WORKSPACE": str(ROOT),
                "GITHUB_OUTPUT": str(output),
                "GITHUB_ENV": str(environment),
                "GITHUB_PATH": str(path_file),
                "RUNNER_TEMP": directory,
                "RUNNER_ENVIRONMENT": "self-hosted",
                "RUNNER_OS": "macOS",
                "RUNNER_ENVIRONMENT_VALUE": "self-hosted",
                "RUNNER_OS_VALUE": "macOS",
                "ENABLED": "true",
                "CARGO_HOME": str(root / "existing-cargo"),
                "CODEX_LOCAL_BUILD_JOBS": "4",
                "CARGO_BUILD_JOBS": "4",
                "CODEX_LOCAL_BUILD_MEMORY_MB": "24576",
                "CODEX_LOCAL_BUILD_LOCK": str(root / "build.lock"),
                "DEFAULT_BRANCH": "main",
                "EXECUTION_MODE": "local",
                "REF_NAME": "main",
                "REF": "refs/heads/main",
                "REPOSITORY": "cbusillo/codex-lab",
                "GITHUB_REF": "refs/heads/main",
                "GITHUB_EVENT_NAME": "workflow_dispatch",
                "WORKFLOW_REF": "cbusillo/codex-lab/.github/workflows/codex-lab-release.yml@refs/heads/main",
                "POLICY_RESULT": "success",
                "AUTHORIZATION_RESULT": "success",
                **overrides,
            }
            result = subprocess.run(
                [executable, "-c", script],
                env=env,
                text=True,
                capture_output=True,
                check=False,
                timeout=10,
            )
            return (
                result.returncode,
                output.read_text().replace(directory, "$RUNNER_TEMP"),
                environment.read_text().replace(directory, "$RUNNER_TEMP"),
                path_file.read_text().replace(directory, "$RUNNER_TEMP"),
                result.stderr,
            )

    def setup_script(self) -> str:
        return shell_block(
            (ROOT / ".github/actions/setup-local-rust-ci/action.yml").read_text(),
            "name: Configure job-scoped Rust tool state",
        )

    def test_local_setup_exports_private_cargo_home_and_marker(self) -> None:
        code, output, environment, path, error = self.run_shell(self.setup_script())
        self.assertEqual(
            (code, output, environment, path),
            (
                0,
                "cargo-home=$RUNNER_TEMP/cargo-home\n",
                "CARGO_HOME=$RUNNER_TEMP/cargo-home\nCARGO_INCREMENTAL=0\nCODEX_LOCAL_RUST_CI=true\n",
                "$RUNNER_TEMP/cargo-home/bin\n",
            ),
            error,
        )

    def test_hosted_setup_preserves_cargo_home_without_local_mutation(self) -> None:
        code, output, environment, path, error = self.run_shell(
            self.setup_script(),
            ENABLED="false",
            CARGO_HOME="/existing/cargo",
            RUNNER_ENVIRONMENT_VALUE="github-hosted",
            RUNNER_OS_VALUE="Linux",
        )
        self.assertEqual(
            (code, output, environment, path),
            (0, "cargo-home=/existing/cargo\n", "", ""),
            error,
        )

    def test_local_setup_rejects_unsafe_identity_and_resource_profiles(self) -> None:
        for override in (
            {"ENABLED": "unexpected"},
            {"RUNNER_ENVIRONMENT_VALUE": "github-hosted"},
            {"RUNNER_OS_VALUE": "Linux"},
            {"CODEX_LOCAL_BUILD_JOBS": ""},
            {"CARGO_BUILD_JOBS": "8"},
            {"CODEX_LOCAL_BUILD_MEMORY_MB": ""},
            {"CODEX_LOCAL_BUILD_LOCK": ""},
            {"CODEX_LOCAL_BUILD_LOCK": "relative.lock"},
        ):
            with self.subTest(override=override):
                code, output, environment, path, _ = self.run_shell(
                    self.setup_script(), **override
                )
                self.assertNotEqual(code, 0)
                self.assertEqual((output, environment, path), ("", "", ""))

    def test_policy_allows_local_only_for_approved_default_branch_callers(self) -> None:
        script = shell_block(
            (ROOT / ".github/workflows/rust-ci-full.yml").read_text(),
            "name: Require an approved execution mode and caller",
        )
        invalid_overrides = (
            {"EXECUTION_MODE": "unknown"},
            {"REPOSITORY": "fork/codex-lab"},
            {"GITHUB_EVENT_NAME": "pull_request"},
            {"GITHUB_EVENT_NAME": "push"},
            {"REF_NAME": "task-branch"},
            {"REF": "refs/heads/task-branch"},
            {
                "GITHUB_REF": "refs/tags/main",
                "REF": "refs/tags/main",
                "WORKFLOW_REF": "cbusillo/codex-lab/.github/workflows/codex-lab-release.yml@refs/tags/main",
            },
            {
                "WORKFLOW_REF": "cbusillo/codex-lab/.github/workflows/full-ci.yml@refs/heads/main"
            },
            {
                "WORKFLOW_REF": "cbusillo/codex-lab/.github/workflows/rust-ci-local.yml@refs/heads/task-branch"
            },
            {
                "WORKFLOW_REF": "fork/codex-lab/.github/workflows/rust-ci-local.yml@refs/heads/main"
            },
            {
                "WORKFLOW_REF": "cbusillo/codex-lab/.github/workflows/rust-ci-full.yml@refs/heads/main"
            },
        )
        for executable, caller in itertools.product(
            ("bash", "/bin/bash"), ("codex-lab-release.yml", "rust-ci-local.yml")
        ):
            workflow_ref = f"cbusillo/codex-lab/.github/workflows/{caller}@refs/heads/main"
            with self.subTest(executable=executable, caller=caller):
                code, output, _, _, error = self.run_shell(
                    script, executable=executable, WORKFLOW_REF=workflow_ref
                )
                self.assertEqual(
                    (code, output),
                    (0, "execution_mode=local\nrunner=macos-codex-lab\n"),
                    error,
                )
                for override in invalid_overrides:
                    with self.subTest(override=override):
                        code, output, _, _, _ = self.run_shell(
                            script,
                            executable=executable,
                            **{"WORKFLOW_REF": workflow_ref, **override},
                        )
                        self.assertNotEqual(code, 0)
                        self.assertEqual(output, "")
        code, output, _, _, error = self.run_shell(
            script,
            EXECUTION_MODE="hosted",
            REPOSITORY="fork/codex-lab",
            GITHUB_EVENT_NAME="pull_request",
            REF_NAME="task-branch",
        )
        self.assertEqual(
            (code, output), (0, "execution_mode=hosted\nrunner=macos-26\n"), error
        )

    def test_execution_gate_never_accepts_missing_or_failed_local_authorization(
        self,
    ) -> None:
        script = shell_block(
            (ROOT / ".github/workflows/rust-ci-full.yml").read_text(),
            "name: Require successful execution policy and authorization",
        )
        results = ("success", "failure", "skipped", "cancelled")
        for executable, mode, policy, authorization in itertools.product(
            ("bash", "/bin/bash"), ("hosted", "local", "unknown"), results, results
        ):
            with self.subTest(
                executable=executable,
                mode=mode,
                policy=policy,
                authorization=authorization,
            ):
                code, _, _, _, error = self.run_shell(
                    script,
                    executable=executable,
                    EXECUTION_MODE=mode,
                    POLICY_RESULT=policy,
                    AUTHORIZATION_RESULT=authorization,
                )
                allowed = policy == "success" and (
                    (mode == "local" and authorization == "success")
                    or (mode == "hosted" and authorization == "skipped")
                )
                self.assertEqual(code == 0, allowed, error)


if __name__ == "__main__":
    unittest.main()
