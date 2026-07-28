#!/usr/bin/env python3
"""Contract tests for the rusty_v8 artifact download shell.

The script builds a download URL from repository-controlled inputs, so the
tests drive it with a stubbed `curl` and assert both the URL it resolves and the
inputs it refuses.
"""

import hashlib
import gzip
import os
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github/scripts/download-rusty-v8-artifacts.sh"
ACTION = ROOT / ".github/actions/setup-rusty-v8/action.yml"
WORKFLOWS = ROOT / ".github/workflows"
TARGET = "x86_64-unknown-linux-musl"
VERSION = "1.2.3"


class DownloadRustyV8Test(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp_dir.cleanup)
        self.root = Path(self.temp_dir.name)
        self.runner_temp = self.root / "runner-temp"
        self.runner_temp.mkdir()
        self.github_env = self.root / "github-env"
        self.github_env.touch()
        self.request_log = self.root / "requests.log"
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.release_dir = self.root / "release"
        self.release_dir.mkdir()
        self.write_release_assets()
        self.write_curl_stub()

    def write_release_assets(self) -> None:
        archive_name = f"librusty_v8_release_{TARGET}.a.gz"
        binding_name = f"src_binding_release_{TARGET}.rs"
        archive_bytes = gzip.compress(b"not-really-v8")
        binding_bytes = b"// bindings\n"
        (self.release_dir / archive_name).write_bytes(archive_bytes)
        (self.release_dir / binding_name).write_bytes(binding_bytes)
        checksums = "".join(
            f"{hashlib.sha256(payload).hexdigest()}  {name}\n"
            for name, payload in (
                (archive_name, archive_bytes),
                (binding_name, binding_bytes),
            )
        )
        (self.release_dir / f"rusty_v8_release_{TARGET}.sha256").write_text(checksums)

    def write_curl_stub(self) -> None:
        """Serve the staged release assets and record every requested URL."""

        curl = self.bin_dir / "curl"
        curl.write_text(
            textwrap.dedent(
                f"""\
                #!/usr/bin/env bash
                set -euo pipefail
                url=""
                output=""
                while [[ "$#" -gt 0 ]]; do
                  case "$1" in
                    -o) output="$2"; shift 2 ;;
                    -*) shift ;;
                    *) url="$1"; shift ;;
                  esac
                done
                echo "$url" >> "{self.request_log}"
                asset="${{url##*/}}"
                source="{self.release_dir}/$asset"
                if [[ ! -f "$source" ]]; then
                  echo "curl: (22) missing $url" >&2
                  exit 22
                fi
                cp "$source" "$output"
                """
            )
        )
        curl.chmod(0o755)

    def run_script(self, **env_overrides: str) -> subprocess.CompletedProcess[str]:
        env = dict(os.environ)
        env["PATH"] = f"{self.bin_dir}{os.pathsep}{env['PATH']}"
        env.update(
            {
                "TARGET": TARGET,
                "RUNNER_TEMP": str(self.runner_temp),
                "GITHUB_ENV": str(self.github_env),
                "GITHUB_REPOSITORY": "cbusillo/codex-lab",
                "GITHUB_SERVER_URL": "https://github.com",
                "RUSTY_V8_CRATE_VERSION": VERSION,
            }
        )
        env.pop("RUSTY_V8_ARTIFACT_REPOSITORY", None)
        for key, value in env_overrides.items():
            if value is None:
                env.pop(key, None)
            else:
                env[key] = value
        return subprocess.run(
            ["bash", str(SCRIPT)], env=env, capture_output=True, text=True
        )

    def requested_urls(self) -> list[str]:
        if not self.request_log.exists():
            return []
        return self.request_log.read_text().split()

    def test_downloads_from_the_current_repository(self) -> None:
        result = self.run_script()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        base = f"https://github.com/cbusillo/codex-lab/releases/download/rusty-v8-v{VERSION}"
        self.assertEqual(
            self.requested_urls(),
            [
                f"{base}/librusty_v8_release_{TARGET}.a.gz",
                f"{base}/src_binding_release_{TARGET}.rs",
                f"{base}/rusty_v8_release_{TARGET}.sha256",
            ],
        )

    def test_retries_transient_download_failures(self) -> None:
        script = SCRIPT.read_text()

        self.assertIn("--retry 5", script)
        self.assertIn("--retry-all-errors", script)
        self.assertIn("--retry-max-time 120", script)

    def test_never_falls_back_to_upstream(self) -> None:
        result = self.run_script()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        for url in self.requested_urls():
            self.assertNotIn("openai/codex", url)

    def test_explicit_artifact_repository_overrides_the_default(self) -> None:
        result = self.run_script(RUSTY_V8_ARTIFACT_REPOSITORY="openai/codex")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        for url in self.requested_urls():
            self.assertTrue(
                url.startswith("https://github.com/openai/codex/releases/download/"),
                url,
            )

    def test_honors_github_server_url(self) -> None:
        result = self.run_script(GITHUB_SERVER_URL="https://ghe.example.com")

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        for url in self.requested_urls():
            self.assertTrue(url.startswith("https://ghe.example.com/"), url)

    def test_exports_artifact_paths(self) -> None:
        result = self.run_script()

        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        exported = dict(
            line.split("=", 1)
            for line in self.github_env.read_text().splitlines()
            if line
        )
        binding_dir = self.runner_temp / "rusty_v8"
        self.assertEqual(
            exported,
            {
                "RUSTY_V8_ARCHIVE": str(
                    binding_dir / f"librusty_v8_release_{TARGET}.a.gz"
                ),
                "RUSTY_V8_SRC_BINDING_PATH": str(
                    binding_dir / f"src_binding_release_{TARGET}.rs"
                ),
            },
        )

    def test_rejects_repository_with_a_path_traversal(self) -> None:
        result = self.run_script(
            RUSTY_V8_ARTIFACT_REPOSITORY="openai/codex/../../evil/repo"
        )

        self.assertNotEqual(0, result.returncode)
        self.assertIn("Invalid rusty_v8 artifact repository", result.stderr)
        self.assertEqual(self.requested_urls(), [])

    def test_rejects_repository_with_a_query_string(self) -> None:
        result = self.run_script(RUSTY_V8_ARTIFACT_REPOSITORY="evil/repo?x=y")

        self.assertNotEqual(0, result.returncode)
        self.assertIn("Invalid rusty_v8 artifact repository", result.stderr)
        self.assertEqual(self.requested_urls(), [])

    def test_rejects_missing_repository(self) -> None:
        result = self.run_script(GITHUB_REPOSITORY="")

        self.assertNotEqual(0, result.returncode)
        self.assertIn("No rusty_v8 artifact repository", result.stderr)
        self.assertEqual(self.requested_urls(), [])

    def test_rejects_non_https_server_url(self) -> None:
        result = self.run_script(GITHUB_SERVER_URL="http://github.com")

        self.assertNotEqual(0, result.returncode)
        self.assertIn("Invalid GITHUB_SERVER_URL", result.stderr)
        self.assertEqual(self.requested_urls(), [])

    def test_rejects_server_url_with_a_path(self) -> None:
        result = self.run_script(GITHUB_SERVER_URL="https://github.com/evil/repo")

        self.assertNotEqual(0, result.returncode)
        self.assertIn("Invalid GITHUB_SERVER_URL", result.stderr)
        self.assertEqual(self.requested_urls(), [])

    def test_rejects_target_with_a_path_segment(self) -> None:
        result = self.run_script(TARGET="../../etc/passwd")

        self.assertNotEqual(0, result.returncode)
        self.assertIn("Invalid rusty_v8 target", result.stderr)
        self.assertEqual(self.requested_urls(), [])

    def test_rejects_a_checksum_file_that_covers_the_wrong_file_count(self) -> None:
        (self.release_dir / f"rusty_v8_release_{TARGET}.sha256").write_text(
            "0" * 64 + f"  librusty_v8_release_{TARGET}.a.gz\n"
        )

        result = self.run_script()

        self.assertNotEqual(0, result.returncode)
        self.assertIn("Expected exactly two checksums", result.stderr)

    def test_rejects_a_tampered_artifact(self) -> None:
        (self.release_dir / f"src_binding_release_{TARGET}.rs").write_bytes(b"tampered")

        result = self.run_script()

        self.assertNotEqual(0, result.returncode)


class SetupRustyV8ActionTest(unittest.TestCase):
    """The action must delegate to the verified script, not inline the URL."""

    def test_action_has_no_hardcoded_upstream_repository(self) -> None:
        self.assertNotIn("openai/codex", ACTION.read_text())

    def test_action_runs_the_download_script(self) -> None:
        self.assertIn("download-rusty-v8-artifacts.sh", ACTION.read_text())


class WorkflowArtifactSourceTest(unittest.TestCase):
    """CI may read upstream artifacts; the release path must not."""

    def test_full_ci_reads_upstream_artifacts_explicitly(self) -> None:
        for workflow_name in (
            "rust-ci-full.yml",
            "rust-ci-full-nextest-platform.yml",
        ):
            with self.subTest(workflow_name=workflow_name):
                workflow = (WORKFLOWS / workflow_name).read_text()
                self.assertIn("artifact-repository: openai/codex", workflow)

    def test_release_keeps_the_fail_closed_default(self) -> None:
        workflow = (WORKFLOWS / "rust-release.yml").read_text()
        self.assertNotIn("artifact-repository", workflow)


if __name__ == "__main__":
    unittest.main()
