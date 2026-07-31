from pathlib import Path
import os
import re
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"
HOOK = ROOT / ".github/scripts/authorize-self-hosted-runner-job.sh"
AUTHORIZATION_WORKFLOW = "uses: ./.github/workflows/authorize-self-hosted.yml"
PERSISTENT_RUNNER_LABELS = (
    "self-hosted",
    "codex-lab-app",
    "codex-lab-linux",
    "macos-codex-lab",
)
SELF_HOSTED_JOBS = {
    "bazel.yml": ("build", "clippy", "verify-release-build"),
    "codex-lab-app.yml": ("build-macos-aarch64",),
    "codex-lab-release.yml": ("build-macos-aarch64",),
    "exec-harness.yml": ("codex-exec-harness",),
    "rust-release-argument-comment-lint.yml": ("build",),
    "rust-release-zsh.yml": ("darwin",),
    "rust-release.yml": ("build", "package-macos", "finalize-macos"),
    "rusty-v8-release.yml": ("build",),
    "sdk-integration.yml": ("typescript-sdk-integration",),
    "v8-canary.yml": ("build",),
}


def persistent_runner_workflows() -> list[Path]:
    workflows = sorted({*WORKFLOWS.glob("*.yml"), *WORKFLOWS.glob("*.yaml")})
    return [
        path
        for path in workflows
        if path.name != "authorize-self-hosted.yml"
        and any(
            label in path.read_text(encoding="utf-8")
            for label in PERSISTENT_RUNNER_LABELS
        )
    ]


def workflow_job_blocks(contents: str) -> dict[str, list[str]]:
    blocks: dict[str, list[str]] = {}
    current_job: str | None = None
    in_jobs = False
    for line in contents.splitlines():
        if line == "jobs:":
            in_jobs = True
            continue
        if not in_jobs:
            continue
        match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if match:
            current_job = match.group(1)
            blocks[current_job] = []
            continue
        if current_job is not None:
            blocks[current_job].append(line)
    return blocks


def job_needs(block: list[str]) -> set[str]:
    needs: set[str] = set()
    reading_list = False
    for line in block:
        match = re.match(r"^    needs:\s*(.*)$", line)
        if match:
            reading_list = not bool(match.group(1))
            inline = match.group(1).strip().strip("[]")
            if inline:
                needs.update(item.strip() for item in inline.split(","))
            continue
        if reading_list:
            item = re.match(r"^      - ([A-Za-z0-9_-]+)\s*$", line)
            if item:
                needs.add(item.group(1))
                continue
            if line.strip():
                reading_list = False
    return needs


def depends_on_authorization(
    job: str,
    blocks: dict[str, list[str]],
    visited: set[str] | None = None,
) -> bool:
    if job == "authorize_self_hosted":
        return True
    visited = set() if visited is None else visited
    if job in visited:
        return False
    visited.add(job)
    return any(
        dependency in blocks
        and depends_on_authorization(dependency, blocks, visited.copy())
        for dependency in job_needs(blocks.get(job, []))
    )


class SelfHostedWorkflowPolicyTest(unittest.TestCase):
    def test_every_persistent_runner_workflow_calls_the_authorization_gate(self) -> None:
        workflows = persistent_runner_workflows()
        self.assertTrue(workflows)

        for path in workflows:
            with self.subTest(workflow=path.name):
                contents = path.read_text(encoding="utf-8")
                self.assertIn(AUTHORIZATION_WORKFLOW, contents)

    def test_every_persistent_runner_job_depends_on_authorization(self) -> None:
        workflows = {path.name: path for path in persistent_runner_workflows()}
        self.assertEqual(set(workflows), set(SELF_HOSTED_JOBS))

        for workflow_name, job_names in SELF_HOSTED_JOBS.items():
            blocks = workflow_job_blocks(
                workflows[workflow_name].read_text(encoding="utf-8")
            )
            for job_name in job_names:
                with self.subTest(workflow=workflow_name, job=job_name):
                    self.assertIn(job_name, blocks)
                    self.assertTrue(depends_on_authorization(job_name, blocks))

    def test_pull_request_workflows_require_same_repository_heads(self) -> None:
        trigger = re.compile(r"^  pull_request:(?: \{\})?$", re.MULTILINE)
        pull_request_workflows = {"codex-lab-app.yml", "v8-canary.yml"}

        for path in persistent_runner_workflows():
            with self.subTest(workflow=path.name):
                contents = path.read_text(encoding="utf-8")
                if path.name in pull_request_workflows:
                    self.assertRegex(contents, trigger)
                    self.assertIn(
                        "github.event.pull_request.head.repo.full_name == github.repository",
                        contents,
                    )
                else:
                    self.assertNotRegex(contents, trigger)

    def test_pull_request_app_build_keeps_the_stacked_pr_trigger(self) -> None:
        contents = (WORKFLOWS / "codex-lab-app.yml").read_text(encoding="utf-8")

        self.assertRegex(contents, re.compile(r"^  pull_request:$", re.MULTILINE))
        self.assertNotRegex(
            contents,
            re.compile(r"^  pull_request_target:$", re.MULTILINE),
        )


class RunnerHostHookTest(unittest.TestCase):
    def run_hook(
        self,
        *,
        repository: str = "cbusillo/codex-lab",
        actor: str = "cbusillo",
        triggering_actor: str = "shiny-code-bot",
    ) -> subprocess.CompletedProcess[str]:
        env = {
            **os.environ,
            "GITHUB_REPOSITORY": repository,
            "GITHUB_ACTOR": actor,
            "GITHUB_TRIGGERING_ACTOR": triggering_actor,
        }
        return subprocess.run(
            ["bash", str(HOOK)],
            check=False,
            capture_output=True,
            env=env,
            text=True,
        )

    def test_allows_each_trusted_actor_pair(self) -> None:
        for actor in ("cbusillo", "shiny-code-bot"):
            for triggering_actor in ("cbusillo", "shiny-code-bot"):
                with self.subTest(actor=actor, triggering_actor=triggering_actor):
                    self.assertEqual(
                        self.run_hook(
                            actor=actor,
                            triggering_actor=triggering_actor,
                        ).returncode,
                        0,
                    )

    def test_denies_an_untrusted_initiating_actor(self) -> None:
        result = self.run_hook(actor="untrusted-contributor")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("initiating actor is not trusted", result.stdout)

    def test_denies_an_untrusted_triggering_actor(self) -> None:
        result = self.run_hook(triggering_actor="untrusted-contributor")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("triggering actor is not trusted", result.stdout)

    def test_denies_an_unexpected_repository(self) -> None:
        result = self.run_hook(repository="cbusillo/other-repository")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unexpected repository", result.stdout)


if __name__ == "__main__":
    unittest.main()
