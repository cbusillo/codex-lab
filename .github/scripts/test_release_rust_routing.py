"""Exercise the actual workflow shell guards without starting any builds."""

import itertools
import os
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

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

    def nextest_tests_script(self, *, local: bool, remote: bool = False) -> str:
        workflow = (
            ROOT / ".github/workflows/rust-ci-full-nextest-platform.yml"
        ).read_text()
        script = shell_block(workflow, "      - name: tests\n")
        replacements = {
            "${{ inputs.use_local_resources }}": "true" if local else "false",
            "${{ inputs.target }}": "aarch64-apple-darwin",
            "${{ inputs.profile }}": "ci-test",
            "${{ inputs.test_threads }}": "8",
            "${{ inputs.remote_env }}": "true" if remote else "false",
            "${{ inputs.remote_test_filter }}": "",
            "${{ github.run_id }}": "test-run",
            "${{ matrix.shard }}": "1",
            "${{ matrix.partition_count }}": "1" if local else "4",
        }
        for source, replacement in replacements.items():
            script = script.replace(source, replacement)
        return script.replace(
            "python3 ../.github/scripts/local_build_resources.py exec --",
            '"${LOCAL_BUILD_WRAPPER}" --',
        )

    def nextest_version_guard_script(self) -> str:
        workflow = (
            ROOT / ".github/workflows/rust-ci-full-nextest-platform.yml"
        ).read_text()
        guard = workflow.split(
            '          nextest_bin="$(command -v cargo-nextest || true)"', 1
        )[1]
        guard = guard.split("          archive_dir=", 1)[0]
        return textwrap.dedent(
            'nextest_bin="$(command -v cargo-nextest || true)"' + guard
        )

    def run_nextest_behavior(
        self, *, local: bool, version: str = "0.9.111", run_status: int = 0
    ) -> tuple[int, str, str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workspace = root / "workspace"
            codex_rs = workspace / "codex-rs"
            (workspace / ".github/scripts").mkdir(parents=True)
            codex_rs.mkdir()
            (workspace / ".github/scripts/local_build_resources.py").symlink_to(
                ROOT / ".github/scripts/local_build_resources.py"
            )
            bin_dir = root / "bin"
            bin_dir.mkdir()
            target_dir = root / "cargo-target"
            target_dir.mkdir()
            helper_dir = root / "helpers"
            helper_dir.mkdir()
            for name in ("codex-code-mode-host", "codex-execve-wrapper"):
                (helper_dir / name).write_text("helper\n")
            mock = bin_dir / "cargo-nextest"
            mock.write_text(
                textwrap.dedent(
                    f"""\
                    #!/bin/bash
                    set -euo pipefail
                    if [[ "${{1:-}}" == "nextest" && "${{2:-}}" == "--version" ]]; then
                      echo "cargo-nextest {version} (mock)"
                      exit 0
                    fi
                    printf '%s\\n' "$*" >> "${{MOCK_LOG}}"
                    profile=default
                    for arg in "$@"; do
                      if [[ "${{previous:-}}" == "--profile" ]]; then profile="$arg"; fi
                      previous="$arg"
                    done
                    mkdir -p "$(pwd)/target/nextest/$profile"
                    printf '<testsuite tests="1"/>\\n' > "$(pwd)/target/nextest/$profile/junit.xml"
                    exit {run_status}
                    """
                )
            )
            mock.chmod(0o755)
            wrapper = root / "wrapper"
            wrapper.write_text('#!/bin/bash\nshift\nexec "$@"\n')
            wrapper.chmod(0o755)
            env = {
                **os.environ,
                "PATH": f"{bin_dir}:{os.environ['PATH']}",
                "RUNNER_TEMP": str(root),
                "CARGO_TARGET_DIR": str(target_dir),
                "TEST_HELPERS_ARTIFACT": "helpers",
                "NEXTEST_ARCHIVE_FILE": "archive.tar.zst",
                "LOCAL_BUILD_WRAPPER": str(wrapper),
                "MOCK_LOG": str(root / "mock.log"),
                "RUNNER_OS": "macOS",
                "REMOTE_TEST_FILTER": "",
            }
            (root / "nextest-archive").mkdir()
            (root / "nextest-archive" / "archive.tar.zst").write_text("archive\n")
            result = subprocess.run(
                ["bash", "-c", self.nextest_tests_script(local=local)],
                cwd=codex_rs,
                env=env,
                text=True,
                capture_output=True,
                check=False,
                timeout=10,
            )
            log = (
                (root / "mock.log").read_text() if (root / "mock.log").exists() else ""
            )
            junit = codex_rs / "target/nextest/default/junit.xml"
            local_junit = codex_rs / "target/nextest/local/junit.xml"
            artifact = root / "nextest-junit/native.xml"
            contents = artifact.read_text() if artifact.exists() else ""
            for path in (junit, local_junit):
                if path.exists():
                    path.unlink()
            return result.returncode, log, contents

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

    def test_nextest_selects_profile_and_junit_store_for_local_mode(self) -> None:
        for local in (False, True):
            with self.subTest(local=local):
                code, log, junit = self.run_nextest_behavior(local=local)
                expected = "local" if local else "default"
                self.assertEqual((code, junit), (0, '<testsuite tests="1"/>\n'))
                self.assertIn(f"--profile {expected}", log)

    def test_nextest_aborts_before_run_on_version_mismatch(self) -> None:
        code, log, junit = self.run_nextest_behavior(local=False, version="0.9.1110")
        self.assertNotEqual(code, 0)
        self.assertEqual((log, junit), ("", ""))

    def test_nextest_windows_guard_preserves_spaces_when_converting_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            nextest = bin_dir / "cargo-nextest"
            nextest.write_text(
                "#!/bin/bash\nprintf 'cargo-nextest 0.9.111 (mock)\\n'\n"
            )
            nextest.chmod(0o755)
            cygpath = bin_dir / "cygpath"
            cygpath.write_text(
                "#!/bin/bash\nprintf 'C:/runner/nextest path/cargo-nextest.exe\\n'\n"
            )
            cygpath.chmod(0o755)
            result = subprocess.run(
                ["bash", "-c", self.nextest_version_guard_script()],
                env={
                    **os.environ,
                    "PATH": f"{bin_dir}:{os.environ['PATH']}",
                    "RUNNER_OS": "Windows",
                },
                text=True,
                capture_output=True,
                check=False,
                timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(
                "using cargo-nextest 0.9.111 at C:/runner/nextest path/cargo-nextest.exe",
                result.stdout,
            )

    def test_nextest_preserves_native_failure_after_junit_copy(self) -> None:
        code, log, junit = self.run_nextest_behavior(local=False, run_status=7)
        self.assertEqual(code, 7)
        self.assertIn("--profile default", log)
        self.assertEqual(junit, '<testsuite tests="1"/>\n')

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
            workflow_ref = (
                f"cbusillo/codex-lab/.github/workflows/{caller}@refs/heads/main"
            )
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
