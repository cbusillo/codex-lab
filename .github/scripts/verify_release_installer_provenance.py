#!/usr/bin/env python3
"""Keep fork releases from shipping installers that point at upstream binaries.

`scripts/install/install.sh` and `scripts/install/install.ps1` resolve every
download from `openai/codex`. `rust-release.yml` stages them as release assets,
so a fork tag would publish an `install.sh` that installs upstream OpenAI
binaries under the fork's release page -- the exact substitution a user
downloading from this repository would not expect.

Rather than duplicating the release logic, this verifier asserts the two
properties that keep it honest:

1. Any workflow step that stages `scripts/install/install.*` as a release asset
   is pinned to the repository those installers actually download from.
2. The Codex Lab release workflow publishes only Codex Lab-owned assets, so the
   fork's own installer/updater surface never carries an upstream artifact.
"""

import re
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"
INSTALLER_SOURCES = (
    ROOT / "scripts/install/install.sh",
    ROOT / "scripts/install/install.ps1",
)
UPSTREAM_REPOSITORY = "openai/codex"
CODEX_LAB_RELEASE_WORKFLOW = "codex-lab-release.yml"
# Assets the Codex Lab release workflow is allowed to publish. Everything here
# is built by that workflow from this repository's own sources.
CODEX_LAB_RELEASE_ASSETS = re.compile(
    r"^(codex-lab-[A-Za-z0-9._-]+\.(zip|json)|SHA256SUMS)$"
)
STAGES_INSTALLER = re.compile(r"scripts/install/install\.(sh|ps1)\b")
RELEASE_ASSET_PATH = re.compile(r"dist/(?P<asset>[A-Za-z0-9._-]+)")
REPOSITORY_GUARD = re.compile(
    r"github\.repository\s*==\s*(?P<quote>['\"])(?P<repository>[^'\"]+)(?P=quote)"
)
STEP_START = re.compile(r"^(?P<indent>\s*)-\s")


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    reason: str


@dataclass
class Step:
    line: int
    lines: list[str]

    @property
    def body(self) -> str:
        return "\n".join(self.lines)

    def is_upstream_guarded(self) -> bool:
        return any(
            match.group("repository") == UPSTREAM_REPOSITORY
            for match in REPOSITORY_GUARD.finditer(self.body)
        )


def parse_steps(contents: str) -> list[Step]:
    """Return every `- ...` list item in a workflow with its raw body.

    Workflow steps are the only list items that carry `run:`/`if:` pairs, and a
    coarse split is enough here: a step that never mentions the installer
    scripts is ignored either way.
    """

    steps: list[Step] = []
    current: Step | None = None
    step_indent: int | None = None

    for number, line in enumerate(contents.splitlines(), start=1):
        if not line.strip():
            if current is not None:
                current.lines.append(line)
            continue
        indent = len(line) - len(line.lstrip())
        match = STEP_START.match(line)
        if match is not None and (step_indent is None or indent <= step_indent):
            step_indent = indent
            current = Step(number, [line])
            steps.append(current)
            continue
        if current is not None and indent > (step_indent or 0):
            current.lines.append(line)
            continue
        current = None

    return steps


def installer_downloads_from(path: Path) -> set[str]:
    """Return the repositories an installer script resolves downloads from."""

    contents = path.read_text()
    return {
        f"{owner}/{name}"
        for owner, name in re.findall(
            r"github\.com/(?:repos/)?([A-Za-z0-9._-]+)/([A-Za-z0-9._-]+)/releases",
            contents,
        )
    }


def find_installer_staging_violations() -> list[Violation]:
    violations: list[Violation] = []

    for installer in INSTALLER_SOURCES:
        if not installer.is_file():
            violations.append(Violation(installer, 0, "installer script is missing"))
            continue
        repositories = installer_downloads_from(installer)
        unexpected = repositories - {UPSTREAM_REPOSITORY}
        if unexpected:
            violations.append(
                Violation(
                    installer,
                    0,
                    "installer downloads from unexpected repositories "
                    f"{sorted(unexpected)}; update this verifier deliberately",
                )
            )

    for path in sorted({*WORKFLOWS.glob("*.yml"), *WORKFLOWS.glob("*.yaml")}):
        for step in parse_steps(path.read_text()):
            if not STAGES_INSTALLER.search(step.body):
                continue
            # Only staging into a release asset directory matters; running the
            # installer or testing it is not a publishing action.
            if "dist/" not in step.body:
                continue
            if step.is_upstream_guarded():
                continue
            violations.append(
                Violation(
                    path,
                    step.line,
                    "stages scripts/install installers as release assets without "
                    f"an {UPSTREAM_REPOSITORY} guard; those installers download "
                    f"from {UPSTREAM_REPOSITORY}",
                )
            )

    return violations


def find_codex_lab_asset_violations() -> list[Violation]:
    path = WORKFLOWS / CODEX_LAB_RELEASE_WORKFLOW
    if not path.is_file():
        return [Violation(path, 0, "Codex Lab release workflow is missing")]

    violations: list[Violation] = []
    contents = path.read_text()
    publishing_steps = [
        step for step in parse_steps(contents) if "gh release create" in step.body
    ]
    if not publishing_steps:
        return [
            Violation(
                path,
                0,
                "no `gh release create` step found; the asset allowlist is no "
                "longer verifying anything",
            )
        ]

    for step in publishing_steps:
        assets = {
            match.group("asset") for match in RELEASE_ASSET_PATH.finditer(step.body)
        }
        if not assets:
            violations.append(
                Violation(path, step.line, "publishes a release with no dist/ assets")
            )
            continue
        for asset in sorted(assets):
            if CODEX_LAB_RELEASE_ASSETS.match(asset):
                continue
            violations.append(
                Violation(
                    path,
                    step.line,
                    f"publishes non-Codex Lab release asset '{asset}'",
                )
            )

    return violations


def find_violations() -> list[Violation]:
    violations = find_installer_staging_violations() + find_codex_lab_asset_violations()
    return sorted(
        violations,
        key=lambda violation: (str(violation.path), violation.line, violation.reason),
    )


def main() -> int:
    violations = find_violations()
    if not violations:
        print("release installers and Codex Lab release assets stay fork-owned")
        return 0

    for violation in violations:
        path = violation.path.relative_to(ROOT)
        location = f"{path}:{violation.line}" if violation.line else str(path)
        print(f"{location}: {violation.reason}")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
