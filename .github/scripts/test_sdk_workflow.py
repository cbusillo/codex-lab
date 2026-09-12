import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/sdk.yml"


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


class SdkWorkflowTest(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = WORKFLOW.read_text(encoding="utf-8")

    def test_builds_and_stages_the_sdk_runtime_pair_with_cargo(self) -> None:
        build = named_step_block(
            self.workflow, "Build and stage codex and the code-mode host with Cargo"
        )

        self.assertIn("cargo build --locked", build)
        self.assertIn("--target aarch64-apple-darwin", build)
        self.assertIn("--profile dev-small", build)
        self.assertIn("-p codex-cli --bin codex", build)
        self.assertIn("-p codex-code-mode-host --bin codex-code-mode-host", build)
        self.assertIn(
            '"${CARGO_TARGET_DIR}/aarch64-apple-darwin/dev-small/codex"', build
        )
        self.assertIn(
            '"${CARGO_TARGET_DIR}/aarch64-apple-darwin/dev-small/codex-code-mode-host"',
            build,
        )
        self.assertIn('"${CARGO_TARGET_DIR}/aarch64-apple-darwin/debug/lib"', build)
        self.assertIn('"${install_dir}/lib"', build)
        self.assertNotIn("run-bazel-ci.sh", build)

    def test_configures_the_pinned_native_dependencies(self) -> None:
        voice = named_step_block(self.workflow, "Configure pinned voice SDK")
        rusty_v8 = named_step_block(
            self.workflow, "Configure rusty_v8 artifact overrides"
        )

        self.assertIn("uses: ./.github/actions/setup-voice-sdk", voice)
        self.assertIn("target: aarch64-apple-darwin", voice)
        self.assertIn("uses: ./.github/actions/setup-rusty-v8", rusty_v8)
        self.assertIn("target: aarch64-apple-darwin", rusty_v8)
        self.assertIn("artifact-repository: openai/codex", rusty_v8)

    def test_only_trusted_main_saves_the_compatible_object_cache(self) -> None:
        policy = named_step_block(self.workflow, "Select compiler cache policy")
        restore = named_step_block(self.workflow, "Restore compiler object cache")
        save = named_step_block(self.workflow, "Save compiler object cache")

        self.assertIn('"push" && "$EVENT_REF" == "refs/heads/main"', policy)
        for component in (
            "v1-sdk-",
            "aarch64-apple-darwin-dev-small-toolchain-",
            "steps.toolchain.outputs.cachekey",
            "steps.voice_sdk.outputs.key",
            "codex-rs/rust-toolchain.toml",
            "codex-rs/Cargo.lock",
            "trusted-main-",
        ):
            self.assertIn(component, restore)
            self.assertIn(component, save)
        self.assertIn("compiler_cache_scope.outputs.may-save == 'true'", save)
        self.assertIn("steps.cargo_build.outcome == 'success'", save)
        self.assertIn("steps.sccache_report.outputs.cache-ready == 'true'", save)

    def test_keeps_the_sdk_commands_and_forced_host_smoke(self) -> None:
        for command in (
            "pnpm install --frozen-lockfile",
            "pnpm -r --filter ./sdk/typescript run build",
            "pnpm -r --filter ./sdk/typescript run lint",
            "pnpm -r --filter ./sdk/typescript run test",
            "uv run --only-group dev --frozen --no-sync pytest",
        ):
            self.assertIn(command, self.workflow)

        self.assertIn('CODEX_SDK_CODE_MODE_HOST_SMOKE: "1"', self.workflow)


if __name__ == "__main__":
    unittest.main()
