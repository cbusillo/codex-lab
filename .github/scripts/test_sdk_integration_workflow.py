import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/sdk-integration.yml"
EXECUTABLE_HOST_TARGET = "//codex-rs/code-mode-host:codex-code-mode-host"
LIBRARY_HOST_TARGET = re.compile(
    r"//codex-rs/code-mode-host:code-mode-host(?![-A-Za-z0-9])"
)


def named_step_block(contents: str, name: str) -> str:
    lines = contents.splitlines()
    marker = f"      - name: {name}"
    try:
        start = lines.index(marker)
    except ValueError as error:
        raise AssertionError(f"workflow step not found: {name}") from error

    end = len(lines)
    for index in range(start + 1, len(lines)):
        if lines[index].startswith("      - name:"):
            end = index
            break
    return "\n".join(lines[start:end])


class SdkIntegrationWorkflowTest(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = WORKFLOW.read_text(encoding="utf-8")

    def test_builds_and_queries_the_executable_host_target(self) -> None:
        build = named_step_block(
            self.workflow, "Build codex and its code-mode host with Bazel"
        )

        self.assertEqual(build.count(EXECUTABLE_HOST_TARGET), 2)
        self.assertIsNone(LIBRARY_HOST_TARGET.search(build))
        self.assertEqual(build.count("//codex-rs/cli:codex"), 2)

    def test_stages_the_pair_through_the_fail_closed_helper(self) -> None:
        build = named_step_block(
            self.workflow, "Build codex and its code-mode host with Bazel"
        )

        self.assertIn("python3 .github/scripts/stage_sdk_bazel_binaries.py", build)
        self.assertIn('--codex-source "${codex_bazel_output_path}"', build)
        self.assertIn('--host-source "${host_bazel_output_path}"', build)
        self.assertIn('--destination "${install_dir}"', build)
        self.assertIn("--expected-architecture arm64", build)
        self.assertNotIn("install -m 755", build)

    def test_exercises_the_staged_sibling_host(self) -> None:
        smoke = named_step_block(
            self.workflow, "Exercise staged code-mode host discovery"
        )

        self.assertIn('CODEX_SDK_CODE_MODE_HOST_SMOKE: "1"', smoke)
        self.assertIn(
            "pnpm --dir sdk/typescript exec jest tests/codeModeHost.test.ts --runInBand",
            smoke,
        )


if __name__ == "__main__":
    unittest.main()
