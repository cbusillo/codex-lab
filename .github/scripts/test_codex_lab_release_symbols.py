#!/usr/bin/env python3
"""Pin the Codex Lab release symbol/strip contract.

Stripping is only correct at one point in the build: after Cargo has emitted a
`.dSYM` sidecar, and before anything signs, bundles, or hashes the engine.
These assertions fail loudly if a future edit reorders those steps or drops the
split-debuginfo override that produces the sidecar in the first place.
"""

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/codex-lab-release.yml"
STEP_NAME = re.compile(r"^\s*-\s+name:\s+(?P<name>.+?)\s*$")
BUILD_STEPS = (
    "Build Codex Lab engine binaries",
    "Archive engine symbols and strip Codex Lab engine",
    "Upload Codex Lab engine symbols",
    "Build Codex Lab app bundle",
    "Sign and verify managed Codex Lab engine binaries",
    "Archive Codex Lab release artifacts",
    "Upload Codex Lab release staging artifact",
)


def step_order(contents: str) -> dict[str, int]:
    order: dict[str, int] = {}
    for number, line in enumerate(contents.splitlines(), start=1):
        match = STEP_NAME.match(line)
        if match is not None:
            order.setdefault(match.group("name"), number)
    return order


class CodexLabReleaseSymbolsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.contents = WORKFLOW.read_text()

    def test_release_build_packs_debug_info_into_a_sidecar(self) -> None:
        self.assertIn(
            "CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO: packed",
            self.contents,
            "the release profile keeps debug info in the binary unless it is packed",
        )

    def test_strip_step_uses_the_shared_symbols_script(self) -> None:
        self.assertIn(
            ".github/scripts/archive-release-symbols-and-strip-binaries.sh",
            self.contents,
        )

    def test_strip_step_repoints_the_engine_binary(self) -> None:
        self.assertIn(
            'echo "CODEX_LAB_BIN=${staged_dir}/codex-lab" >> "$GITHUB_ENV"',
            self.contents,
            "later steps must consume the stripped engine",
        )

    def test_symbols_archive_accepts_cargo_hashed_dwarf_filename(self) -> None:
        self.assertIn(
            "for binary in codex-lab codex-code-mode-host; do",
            self.contents,
        )
        self.assertIn(
            'dwarf_prefix="codex-symbols-${SYMBOLS_ARTIFACT_NAME}/${binary}.dSYM/Contents/Resources/DWARF/"',
            self.contents,
        )
        self.assertIn('suffix !~ /\\//', self.contents)
        self.assertNotIn(
            "Contents/Resources/DWARF/codex-lab\"",
            self.contents,
            "Cargo names the packed DWARF file after its hashed crate artifact",
        )

    def test_symbols_are_archived_before_signing_and_packaging(self) -> None:
        order = step_order(self.contents)
        missing = [name for name in BUILD_STEPS if name not in order]
        self.assertEqual(missing, [], f"missing release steps: {missing}")

        lines = [order[name] for name in BUILD_STEPS]
        self.assertEqual(
            lines,
            sorted(lines),
            "release steps are out of order: "
            + ", ".join(f"{name}@{order[name]}" for name in BUILD_STEPS),
        )


if __name__ == "__main__":
    unittest.main()
