#!/usr/bin/env python3
"""Keep inherited upstream publishing jobs from running in this fork.

Two families of inherited jobs mutate OpenAI-owned state:

* `r2-release.yml` mirrors `openai/codex` GitHub Release assets into the
  upstream Cloudflare R2 `releases` bucket. Nothing in that workflow is
  fork-aware: it downloads from a hard-coded upstream repository and uploads
  with whatever R2 credentials the calling repository provides. Running it here
  would republish upstream assets under Codex Lab credentials, so every job that
  calls it -- and every job inside it -- must be pinned to the upstream
  repository.
* Jobs that publish to an OpenAI-owned external registry or endpoint: the
  `@openai` npm scope, the `OpenAI.Codex` WinGet manifest via the
  `openai-oss-forks` winget-pkgs fork, and the developers.openai.com Vercel
  deploy hook. These are found by the fingerprints below rather than by job
  name, so renaming or copying a job cannot drop its guard.
"""

import re
from dataclasses import dataclass
from dataclasses import field
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
UPSTREAM_REPOSITORY = "openai/codex"
# Reusable workflows that only make sense in the repository that owns the
# upstream release buckets and credentials.
UPSTREAM_ONLY_WORKFLOWS = ("r2-release.yml",)
# Fingerprints of an external mutation to something OpenAI owns. A job body that
# matches any of these must be pinned to the upstream repository.
UPSTREAM_OWNED_MUTATIONS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("the @openai npm scope", re.compile(r"""scope\s*:\s*["']?@openai\b""")),
    ("the OpenAI.Codex WinGet manifest", re.compile(r"\bOpenAI\.Codex\b")),
    ("the openai-oss-forks winget-pkgs fork", re.compile(r"\bopenai-oss-forks\b")),
    ("the WinGet publish credential", re.compile(r"\bWINGET_PUBLISH_PAT\b")),
    (
        "the developers.openai.com deploy hook",
        re.compile(r"\bDEV_WEBSITE_VERCEL_DEPLOY_HOOK_URL\b"),
    ),
    (
        "the upstream R2 release bucket credential",
        re.compile(r"\bR2_RELEASES_[A-Z0-9_]+\b"),
    ),
)
JOBS_KEY = re.compile(r"^jobs\s*:\s*$")
JOB_NAME = re.compile(r"^(?P<name>[A-Za-z_][A-Za-z0-9_-]*)\s*:\s*$")
KEY = re.compile(r"^(?P<key>[A-Za-z_][A-Za-z0-9_-]*)\s*:(?P<value>.*)$")
LOCAL_WORKFLOW = re.compile(r"\./\.github/workflows/(?P<name>[^#\s'\"]+)")
REPOSITORY_GUARD = re.compile(
    r"github\.repository\s*==\s*(?P<quote>['\"])(?P<repository>[^'\"]+)(?P=quote)"
)


@dataclass(frozen=True)
class Violation:
    path: Path
    line: int
    reason: str


@dataclass
class Job:
    name: str
    line: int
    lines: list[str] = field(default_factory=list)


def parse_jobs(contents: str) -> list[Job]:
    """Return the top-level jobs of a workflow with their raw bodies."""

    jobs: list[Job] = []
    in_jobs = False
    job_indent: int | None = None
    current: Job | None = None

    for number, line in enumerate(contents.splitlines(), start=1):
        stripped = line.strip()
        if not in_jobs:
            in_jobs = bool(JOBS_KEY.match(line))
            continue
        if stripped and not line[:1].isspace():
            break
        if not stripped or stripped.startswith("#"):
            if current is not None:
                current.lines.append(line)
            continue

        indent = len(line) - len(line.lstrip())
        name = JOB_NAME.match(stripped)
        if name is not None and (job_indent is None or indent == job_indent):
            job_indent = indent
            current = Job(name.group("name"), number)
            jobs.append(current)
            continue
        if current is not None:
            current.lines.append(line)

    return jobs


def job_key_value(job: Job, key: str) -> str | None:
    """Return the text of a job-level key, including continuation lines."""

    body = [line for line in job.lines if line.strip()]
    if not body:
        return None
    key_indent = min(len(line) - len(line.lstrip()) for line in body)

    collected: list[str] = []
    capturing = False
    for line in job.lines:
        stripped = line.strip()
        if not stripped:
            if capturing:
                collected.append(stripped)
            continue
        indent = len(line) - len(line.lstrip())
        if indent == key_indent:
            match = KEY.match(stripped)
            if match is None:
                capturing = False
                continue
            if match.group("key") != key:
                if capturing:
                    break
                continue
            capturing = True
            collected.append(match.group("value").strip())
            continue
        if capturing:
            collected.append(stripped)

    return " ".join(collected).strip() if capturing or collected else None


def has_upstream_repository_guard(job: Job) -> bool:
    condition = job_key_value(job, "if")
    if condition is None:
        return False
    return any(
        match.group("repository") == UPSTREAM_REPOSITORY
        for match in REPOSITORY_GUARD.finditer(condition)
    )


def upstream_owned_mutations(job: Job) -> list[str]:
    """Return the OpenAI-owned mutations this job's body fingerprints as."""

    body = "\n".join(job.lines)
    return [
        description
        for description, pattern in UPSTREAM_OWNED_MUTATIONS
        if pattern.search(body)
    ]


def called_local_workflows(job: Job) -> set[str]:
    uses = job_key_value(job, "uses")
    if uses is None:
        return set()
    return {match.group("name") for match in LOCAL_WORKFLOW.finditer(uses)}


def find_violations(workflows_dir: Path) -> list[Violation]:
    violations: list[Violation] = []
    upstream_only = set(UPSTREAM_ONLY_WORKFLOWS)

    for name in sorted(upstream_only):
        path = workflows_dir / name
        if not path.is_file():
            violations.append(Violation(path, 0, "upstream-only workflow is missing"))
            continue
        for job in parse_jobs(path.read_text()):
            if not has_upstream_repository_guard(job):
                violations.append(
                    Violation(
                        path,
                        job.line,
                        f"job '{job.name}' runs upstream-only publishing without an "
                        f"{UPSTREAM_REPOSITORY} guard",
                    )
                )

    paths = sorted({*workflows_dir.glob("*.yml"), *workflows_dir.glob("*.yaml")})
    for path in paths:
        for job in parse_jobs(path.read_text()):
            guarded = has_upstream_repository_guard(job)
            if not guarded and path.name not in upstream_only:
                called = called_local_workflows(job) & upstream_only
                if called:
                    violations.append(
                        Violation(
                            path,
                            job.line,
                            f"job '{job.name}' calls {sorted(called)[0]} without an "
                            f"{UPSTREAM_REPOSITORY} guard",
                        )
                    )
            if guarded:
                continue
            for mutation in upstream_owned_mutations(job):
                violations.append(
                    Violation(
                        path,
                        job.line,
                        f"job '{job.name}' publishes to {mutation} without an "
                        f"{UPSTREAM_REPOSITORY} guard",
                    )
                )

    return sorted(
        violations,
        key=lambda violation: (str(violation.path), violation.line, violation.reason),
    )


def main() -> int:
    violations = find_violations(ROOT / ".github/workflows")
    if not violations:
        print("upstream-only release publishing is pinned to the upstream repository")
        return 0

    for violation in violations:
        path = violation.path.relative_to(ROOT)
        location = f"{path}:{violation.line}" if violation.line else str(path)
        print(f"{location}: {violation.reason}")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
