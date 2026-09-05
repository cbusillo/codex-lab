"""Behavioral coverage for the blocking Rust compiler-object cache."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_TEXT = (REPO_ROOT / ".github/workflows/rust-ci.yml").read_text(
    encoding="utf-8"
)


def step_script(name: str) -> str:
    lines = WORKFLOW_TEXT.splitlines()
    marker = f"      - name: {name}"
    step_start = lines.index(marker)
    run_start = next(
        index
        for index in range(step_start + 1, len(lines))
        if lines[index] == "        run: |"
    )
    script_lines: list[str] = []
    for line in lines[run_start + 1 :]:
        if line and not line.startswith("          "):
            break
        script_lines.append(line[10:] if line else "")
    return "\n".join(script_lines)


def run_step(
    script: str,
    environment: dict[str, str],
    *,
    fake_sccache_statuses: tuple[int, int] | None = None,
) -> tuple[subprocess.CompletedProcess[str], dict[str, str]]:
    with tempfile.TemporaryDirectory() as temporary_directory:
        root = Path(temporary_directory)
        github_env = root / "github-env"
        github_output = root / "github-output"
        process_environment = {
            **os.environ,
            **environment,
            "GITHUB_ENV": str(github_env),
            "GITHUB_OUTPUT": str(github_output),
            "GITHUB_STEP_SUMMARY": str(root / "github-summary"),
            "GITHUB_RUN_ID": "9001",
            "RUNNER_TEMP": str(root / "runner-temp"),
        }
        if fake_sccache_statuses is not None:
            binary_dir = root / "bin"
            binary_dir.mkdir()
            fake_sccache = binary_dir / "sccache"
            fake_sccache.write_text(
                "#!/bin/bash\n"
                "[[ $1 == --show-stats ]] && { echo 'Compile requests 10'; "
                'exit "$FAKE_STATS_STATUS"; }\n'
                "echo 'sccache stopped'\n"
                'exit "$FAKE_STOP_STATUS"\n',
                encoding="utf-8",
            )
            fake_sccache.chmod(0o755)
            process_environment.update(
                {
                    "PATH": f"{binary_dir}:{process_environment['PATH']}",
                    "FAKE_STATS_STATUS": str(fake_sccache_statuses[0]),
                    "FAKE_STOP_STATUS": str(fake_sccache_statuses[1]),
                }
            )
        result = subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c", script],
            check=False,
            env=process_environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        return result, {
            "env": github_env.read_text() if github_env.exists() else "",
            "output": github_output.read_text() if github_output.exists() else "",
            "runner_temp": process_environment["RUNNER_TEMP"],
        }


class BlockingCompilerCachePolicyTest(unittest.TestCase):
    def test_scope_script_selects_trusted_and_ephemeral_namespaces(self) -> None:
        cases = (
            ("push", "refs/heads/main", "", "trusted-main"),
            ("pull_request", "refs/pull/42/merge", "42", "pr-42"),
            ("push", "refs/heads/topic", "", "ephemeral-9001"),
            ("workflow_dispatch", "refs/heads/main", "", "ephemeral-9001"),
            ("pull_request", "refs/pull/42/merge", "", None),
        )
        for event, ref, pr_number, expected_scope in cases:
            with self.subTest(event=event, ref=ref):
                result, evidence = run_step(
                    step_script("Select compiler cache write scope"),
                    {"EVENT_NAME": event, "EVENT_REF": ref, "PR_NUMBER": pr_number},
                )
                if expected_scope is None:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("missing pull request number", result.stderr)
                    self.assertNotIn("write-scope=", evidence["output"])
                    continue
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"write-scope={expected_scope}\n", evidence["output"])
                self.assertEqual(
                    evidence["env"],
                    f"SCCACHE_DIR={evidence['runner_temp']}/sccache\n",
                )

    def test_reporting_handles_stats_and_shutdown_failures(self) -> None:
        cases = (
            ((9, 0), 9, "cache-ready=true\n", "Compile requests 10"),
            ((0, 7), 7, "", "cache save is disabled"),
        )
        for statuses, returncode, output, diagnostic in cases:
            with self.subTest(statuses=statuses):
                result, evidence = run_step(
                    step_script("Report sccache statistics"),
                    {},
                    fake_sccache_statuses=statuses,
                )
                self.assertEqual(result.returncode, returncode)
                self.assertIn(diagnostic, result.stdout)
                self.assertEqual(evidence["output"], output)

    def test_restore_and_save_keep_compatibility_and_trust_boundaries(self) -> None:
        workspace_job = WORKFLOW_TEXT.split("  workspace_check:\n", 1)[1].split(
            "\n  argument_comment_lint_package:", 1
        )[0]
        restore = workspace_job.split(
            "      - name: Restore compiler object cache\n", 1
        )[1].split("      - name: Install sccache\n", 1)[0]
        restore_keys = [
            line.strip()
            for line in restore.split("          restore-keys: |\n", 1)[1].splitlines()
            if line.strip()
        ]

        self.assertEqual(len(restore_keys), 2)
        for key in restore_keys:
            self.assertIn("${{ runner.arch }}", key)
            self.assertIn("aarch64-apple-darwin-dev-toolchain-", key)
            self.assertIn("${{ steps.toolchain.outputs.cachekey }}", key)
            self.assertIn("hashFiles('codex-rs/rust-toolchain.toml')", key)
            self.assertIn("hashFiles('codex-rs/Cargo.lock')", key)
        self.assertIn("outputs.write-scope", restore_keys[0])
        self.assertTrue(restore_keys[1].endswith("-trusted-main-"))
        self.assertIn("steps.workspace_compile.outcome == 'success'", workspace_job)
        self.assertIn(
            "steps.sccache_report.outputs.cache-ready == 'true'", workspace_job
        )
        self.assertIn(
            "!startsWith(steps.compiler_cache_scope.outputs.write-scope, 'ephemeral-')",
            workspace_job,
        )
        self.assertIn("-- cargo check --workspace --tests --locked", workspace_job)
        self.assertNotIn("SCCACHE_GHA_ENABLED", workspace_job)


if __name__ == "__main__":
    unittest.main()
