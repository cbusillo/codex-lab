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
    fake_date: str | None = None,
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
        if fake_date is not None or fake_sccache_statuses is not None:
            binary_dir = root / "bin"
            binary_dir.mkdir()
            process_environment["PATH"] = f"{binary_dir}:{process_environment['PATH']}"
        if fake_date is not None:
            fake_date_command = binary_dir / "date"
            fake_date_command.write_text(
                "#!/bin/bash\nprintf '%s\\n' \"$FAKE_DATE\"\n",
                encoding="utf-8",
            )
            fake_date_command.chmod(0o755)
            process_environment["FAKE_DATE"] = fake_date
        if fake_sccache_statuses is not None:
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
                    "FAKE_STATS_STATUS": str(fake_sccache_statuses[0]),
                    "FAKE_STOP_STATUS": str(fake_sccache_statuses[1]),
                }
            )
        result = subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c", script],
            check=False,
            capture_output=True,
            env=process_environment,
            text=True,
        )
        return result, {
            "env": github_env.read_text() if github_env.exists() else "",
            "output": github_output.read_text() if github_output.exists() else "",
            "runner_temp": process_environment["RUNNER_TEMP"],
        }


class BlockingCompilerCachePolicyTest(unittest.TestCase):
    def test_policy_script_selects_daily_main_writes(self) -> None:
        cases = (
            ("push", "refs/heads/main", "true"),
            ("pull_request", "refs/pull/42/merge", "false"),
            ("push", "refs/heads/topic", "false"),
            ("workflow_dispatch", "refs/heads/main", "false"),
        )
        for event, ref, may_save in cases:
            with self.subTest(event=event, ref=ref):
                result, evidence = run_step(
                    step_script("Select compiler cache policy"),
                    {
                        "EVENT_NAME": event,
                        "EVENT_REF": ref,
                    },
                    fake_date="20260906",
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    evidence["output"],
                    f"cache-day=20260906\nmay-save={may_save}\n",
                )
                self.assertEqual(
                    evidence["env"],
                    f"SCCACHE_DIR={evidence['runner_temp']}/sccache\n",
                )

    def test_reporting_handles_stats_and_shutdown_failures(self) -> None:
        cases = (
            ((9, 0), 9, "cache-ready=true\n", "Compile requests 10"),
            ((0, 7), 7, "", "cache save is disabled"),
        )
        for statuses, return_code, output, diagnostic in cases:
            with self.subTest(statuses=statuses):
                result, evidence = run_step(
                    step_script("Report sccache statistics"),
                    {},
                    fake_sccache_statuses=statuses,
                )
                self.assertEqual(result.returncode, return_code)
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
        restore_key = next(
            line.strip().removeprefix("key: ")
            for line in restore.splitlines()
            if line.strip().startswith("key: ")
        )
        save = workspace_job.split("      - name: Save compiler object cache\n", 1)[
            1
        ].split("      - name: Upload Rust feedback evidence\n", 1)[0]
        save_key = next(
            line.strip().removeprefix("key: ")
            for line in save.splitlines()
            if line.strip().startswith("key: ")
        )

        self.assertEqual(restore_key, save_key)
        self.assertTrue(
            restore_key.endswith(
                "-trusted-main-${{ steps.compiler_cache_scope.outputs.cache-day }}"
            )
        )
        self.assertEqual(len(restore_keys), 1)
        for key in (restore_key, *restore_keys):
            self.assertIn("${{ runner.arch }}", key)
            self.assertIn("aarch64-apple-darwin-dev-toolchain-", key)
            self.assertIn("${{ steps.toolchain.outputs.cachekey }}", key)
            self.assertIn("hashFiles('codex-rs/rust-toolchain.toml')", key)
            self.assertIn("hashFiles('codex-rs/Cargo.lock')", key)
        self.assertTrue(restore_keys[0].endswith("-trusted-main-"))
        self.assertNotIn("github.run_id", restore_key)
        self.assertIn(
            "steps.compiler_cache_scope.outputs.may-save == 'true'", workspace_job
        )
        self.assertIn("steps.workspace_compile.outcome == 'success'", workspace_job)
        self.assertIn(
            "steps.sccache_report.outputs.cache-ready == 'true'", workspace_job
        )
        self.assertIn("-- cargo check --workspace --tests --locked", workspace_job)
        self.assertNotIn("SCCACHE_GHA_ENABLED", workspace_job)


if __name__ == "__main__":
    unittest.main()
