#!/usr/bin/env python3
"""Collect deterministic, read-only evidence for an upstream sync checkpoint."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


SCHEMA_VERSION = 1
DEFAULT_TIMEOUT_SECONDS = 300
DEFAULT_MAX_COMMITS = 2_000
MAX_ERROR_CHARS = 4_000
READ_ONLY_SOURCE_COMMANDS = {"config", "rev-parse"}
TRUE_VALUES = {"1", "on", "true", "yes"}
FULL_SHA = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
GIT_LOCAL_ENV_VARS = (
    "GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT "
    "GIT_OBJECT_DIRECTORY GIT_DIR GIT_WORK_TREE GIT_IMPLICIT_WORK_TREE GIT_GRAFT_FILE "
    "GIT_INDEX_FILE GIT_NO_REPLACE_OBJECTS GIT_REPLACE_REF_BASE GIT_PREFIX "
    "GIT_SHALLOW_FILE GIT_COMMON_DIR GIT_NAMESPACE"
).split()
CODEX_BUCKETS = {
    "app-server": "app_server",
    "app-server-protocol": "app_server",
    "core": "core",
    "protocol": "history_storage_protocol",
    "rollout": "history_storage_protocol",
    "state": "history_storage_protocol",
    "thread-store": "history_storage_protocol",
    "tui": "tui",
}
BUCKET_ORDER = (
    "app_server",
    "core",
    "tui",
    "history_storage_protocol",
    "other_codex_rs",
    "repository_tooling",
    "other",
)


AuditError = RuntimeError
Config = argparse.Namespace


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("value must be greater than zero")
    return parsed


def parse_args() -> Config:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=Path)
    for option in (
        "upstream-url",
        "upstream-branch",
        "implementation-baseline",
        "classified-checkpoint",
    ):
        parser.add_argument(f"--{option}", required=True)
    parser.add_argument("--adopted-checkpoint")
    parser.add_argument(
        "--command-timeout-seconds",
        dest="timeout_seconds",
        type=positive_int,
        default=DEFAULT_TIMEOUT_SECONDS,
    )
    parser.add_argument(
        "--max-commits",
        type=positive_int,
        default=DEFAULT_MAX_COMMITS,
    )
    return Config(**vars(parser.parse_args()))


def error_detail(output: str, config: Config) -> str:
    redacted = output
    for secret in (config.upstream_url, str(config.repo), str(config.repo.resolve())):
        redacted = redacted.replace(secret, "<redacted>")
    redacted = re.sub(
        r"(?i)([a-z][a-z0-9+.-]*://)[^/@\s]+@", r"\1<redacted>@", redacted
    )
    if len(redacted) > MAX_ERROR_CHARS:
        redacted = f"{redacted[:MAX_ERROR_CHARS]}…"
    return redacted.strip()


def git_environment(source: bool) -> dict[str, str]:
    environment = os.environ.copy()
    for name in GIT_LOCAL_ENV_VARS:
        environment.pop(name, None)
    environment.update({"GIT_TERMINAL_PROMPT": "0", "LC_ALL": "C"})
    if source:
        environment.update({"GIT_NO_LAZY_FETCH": "1", "GIT_OPTIONAL_LOCKS": "0"})
    return environment


def git_process(
    cwd: Path,
    arguments: list[str],
    config: Config,
    *,
    source: bool = False,
) -> subprocess.CompletedProcess[str]:
    if source and (not arguments or arguments[0] not in READ_ONLY_SOURCE_COMMANDS):
        command = arguments[0] if arguments else "<missing>"
        raise AuditError(f"source repository git command is not read-only: {command}")
    try:
        return subprocess.run(
            ["git", *arguments],
            cwd=cwd,
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=config.timeout_seconds,
            env=git_environment(source),
        )
    except subprocess.TimeoutExpired as error:
        raise AuditError(
            f"git {arguments[0]} exceeded the {config.timeout_seconds}-second timeout"
        ) from error
    except OSError as error:
        raise AuditError(f"git {arguments[0]} could not start: {error}") from error


def git(
    cwd: Path,
    arguments: list[str],
    config: Config,
    description: str,
    *,
    source: bool = False,
) -> str:
    completed = git_process(cwd, arguments, config, source=source)
    if completed.returncode != 0:
        detail = error_detail(completed.stderr or completed.stdout, config)
        raise AuditError(f"{description}{f': {detail}' if detail else ''}")
    return completed.stdout


def full_sha(value: str, label: str) -> str:
    normalized = value.lower()
    if not FULL_SHA.fullmatch(normalized):
        raise AuditError(f"{label} must be a full 40- or 64-character commit id")
    return normalized


def source_query(config: Config, repo: Path, description: str, *arguments: str) -> str:
    return git(repo, list(arguments), config, description, source=True)


def resolve_source(config: Config) -> tuple[Path, Path, str, str]:
    requested = config.repo.expanduser().resolve()
    if not requested.exists():
        raise AuditError(f"repository path does not exist: {requested}")
    repo = Path(
        source_query(
            config,
            requested,
            "repository path is not inside a Git worktree",
            "rev-parse",
            "--show-toplevel",
        ).strip()
    )
    config.repo = repo
    shallow = source_query(
        config,
        repo,
        "failed to inspect repository depth",
        "rev-parse",
        "--is-shallow-repository",
    ).strip()
    if shallow == "true":
        raise AuditError("implementation repository is shallow; hydrate it first")
    promisor = git_process(
        repo,
        [
            "config",
            "--get-regexp",
            r"^(extensions\.partialclone|remote\..*\.(promisor|partialclonefilter))$",
        ],
        config,
        source=True,
    )
    if promisor.returncode not in {0, 1}:
        raise AuditError("failed to inspect repository promisor configuration")
    for line in promisor.stdout.splitlines():
        key, _, value = line.partition(" ")
        if not key.endswith(".promisor") or value.lower() in TRUE_VALUES:
            raise AuditError(
                "implementation repository is a promisor clone; hydrate it first"
            )
    baseline = source_query(
        config,
        repo,
        "implementation baseline does not resolve to a commit",
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"{config.implementation_baseline}^{{commit}}",
    ).strip()
    objects = Path(
        source_query(
            config,
            repo,
            "failed to resolve repository object directory",
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ).strip()
    )
    if any(character in str(objects) for character in "\r\n"):
        raise AuditError("repository object directory contains an unsupported newline")
    baseline = full_sha(baseline, "resolved implementation baseline")
    object_format = "sha1" if len(baseline) == 40 else "sha256"
    return repo, objects, baseline, object_format


def fetch_head(audit_repo: Path, config: Config) -> tuple[str, str]:
    branch = config.upstream_branch.strip()
    if not branch:
        raise AuditError("upstream branch must not be empty")
    git(
        audit_repo,
        ["check-ref-format", "--branch", branch],
        config,
        "upstream branch is not valid",
    )
    git(
        audit_repo,
        ["fetch", "--no-tags", config.upstream_url, f"refs/heads/{branch}"],
        config,
        "failed to fetch live upstream head",
    )
    head = git(
        audit_repo,
        ["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
        config,
        "failed to resolve fetched upstream head",
    ).strip()
    return branch, full_sha(head, "observed upstream head")


def ensure_commit(
    audit_repo: Path,
    commit: str,
    label: str,
    config: Config,
) -> None:
    object_type = git_process(audit_repo, ["cat-file", "-t", commit], config)
    if object_type.returncode == 0:
        resolved_type = object_type.stdout.strip()
        if resolved_type != "commit":
            raise AuditError(f"{label} resolves to {resolved_type}, not a commit")
        return
    detail = error_detail(object_type.stderr or object_type.stdout, config)
    suffix = f": {detail}" if detail and object_type.returncode != 128 else ""
    raise AuditError(
        f"{label} is not available from the fetched upstream branch{suffix}"
    )


def ancestor(audit_repo: Path, older: str, newer: str, config: Config) -> bool:
    completed = git_process(
        audit_repo, ["merge-base", "--is-ancestor", older, newer], config
    )
    if completed.returncode in {0, 1}:
        return completed.returncode == 0
    raise AuditError("failed to compare upstream ancestry")


def bucket_for_path(path: str) -> str:
    parts = path.split("/", maxsplit=2)
    if parts[0] == "codex-rs":
        return (
            CODEX_BUCKETS.get(parts[1], "other_codex_rs")
            if len(parts) > 1
            else "other_codex_rs"
        )
    if parts[0] in {".github", "scripts", "sdk", "tools"}:
        return "repository_tooling"
    return "other"


def primary_bucket(paths: set[str]) -> str:
    if not paths:
        return "other"
    counts = {bucket: 0 for bucket in BUCKET_ORDER}
    for path in paths:
        counts[bucket_for_path(path)] += 1
    return max(
        BUCKET_ORDER, key=lambda bucket: (counts[bucket], -BUCKET_ORDER.index(bucket))
    )


def changed_paths(audit_repo: Path, commit: str, config: Config) -> set[str]:
    output = git(
        audit_repo,
        [
            "diff-tree",
            "--root",
            "--first-parent",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-z",
            commit,
        ],
        config,
        f"failed to inspect changed paths for {commit}",
    )
    return {path for path in output.split("\0") if path}


def patch_counts(
    audit_repo: Path,
    baseline: str,
    classified: str,
    observed_head: str,
    commit_count: int,
    config: Config,
) -> dict[str, int | str]:
    non_reachable = int(
        git(
            audit_repo,
            [
                "rev-list",
                "--count",
                f"{classified}..{observed_head}",
                "--not",
                baseline,
            ],
            config,
            "failed to count upstream commits absent from the implementation baseline",
        ).strip()
    )
    output = git(
        audit_repo,
        ["cherry", baseline, observed_head, classified],
        config,
        "failed to calculate patch equivalence",
    )
    markers = [line.partition(" ")[0] for line in output.splitlines()]
    if any(marker not in {"+", "-"} for marker in markers):
        raise AuditError("git cherry returned an unexpected result")
    patch_equivalent = markers.count("-")
    missing = markers.count("+")
    patch_comparable = patch_equivalent + missing
    if non_reachable > commit_count or patch_comparable > non_reachable:
        raise AuditError("patch-equivalence count exceeds the upstream range")
    return {
        "algorithm": "git-reachability-and-cherry-v1",
        "equivalenceScope": "exact_commit_or-git-cherry-patch-only",
        "exactCommitCount": commit_count - non_reachable,
        "missingPatchCount": missing,
        "patchEquivalentCommitCount": patch_equivalent,
        "uncomparableCommitCount": non_reachable - patch_comparable,
    }


def collect(config: Config) -> dict[str, object]:
    repo, objects, baseline, object_format = resolve_source(config)
    classified = full_sha(config.classified_checkpoint, "classified checkpoint")
    adopted = (
        full_sha(config.adopted_checkpoint, "adopted checkpoint")
        if config.adopted_checkpoint
        else None
    )
    for label, commit in (
        ("classified checkpoint", classified),
        ("adopted checkpoint", adopted),
    ):
        if commit and len(commit) != len(baseline):
            raise AuditError(
                f"{label} does not match the source repository object format"
            )
    with tempfile.TemporaryDirectory(prefix="codex-upstream-audit-") as temp_dir:
        audit_repo = Path(temp_dir) / "audit.git"
        git(
            Path(temp_dir),
            ["init", "--bare", f"--object-format={object_format}", str(audit_repo)],
            config,
            "failed to initialize audit repository",
        )
        (audit_repo / "objects" / "info" / "alternates").write_text(
            f"{objects}\n", encoding="utf-8"
        )
        branch, observed_head = fetch_head(audit_repo, config)
        if len(observed_head) != len(baseline):
            raise AuditError(
                "observed upstream head does not match the source repository object format"
            )
        ensure_commit(audit_repo, classified, "classified checkpoint", config)
        if not ancestor(audit_repo, classified, observed_head, config):
            raise AuditError(
                "classified checkpoint is not an ancestor of the observed upstream head"
            )
        if adopted:
            ensure_commit(audit_repo, adopted, "adopted checkpoint", config)
            if not ancestor(audit_repo, adopted, classified, config):
                raise AuditError(
                    "adopted checkpoint is not an ancestor of the classified checkpoint"
                )
        merge_base = git(
            audit_repo,
            ["merge-base", baseline, observed_head],
            config,
            "implementation baseline and observed upstream head have no merge base",
        ).strip()
        commit_count = int(
            git(
                audit_repo,
                ["rev-list", "--count", f"{classified}..{observed_head}"],
                config,
                "failed to count upstream commits",
            ).strip()
        )
        if commit_count > config.max_commits:
            raise AuditError(
                f"upstream range has {commit_count} commits, exceeding the configured maximum of {config.max_commits}"
            )
        commits = git(
            audit_repo,
            ["rev-list", "--reverse", "--topo-order", f"{classified}..{observed_head}"],
            config,
            "failed to enumerate upstream commits",
        ).splitlines()
        if len(commits) != commit_count:
            raise AuditError(
                "enumerated upstream commit count does not match the summary"
            )
        buckets = {bucket: 0 for bucket in BUCKET_ORDER}
        for commit in commits:
            buckets[primary_bucket(changed_paths(audit_repo, commit, config))] += 1
        return {
            "delta": {
                "commitCount": commit_count,
                "patchEquivalence": patch_counts(
                    audit_repo,
                    baseline,
                    classified,
                    observed_head,
                    commit_count,
                    config,
                ),
                "primaryPathBuckets": buckets,
            },
            "implementation": {
                "baseline": baseline,
                "mergeBaseWithObservedUpstream": merge_base,
            },
            "schemaVersion": SCHEMA_VERSION,
            "upstream": {
                "adoptedCheckpoint": adopted,
                "branch": branch,
                "classifiedCheckpoint": classified,
                "observedHead": observed_head,
            },
        }


def main() -> int:
    config = parse_args()
    try:
        payload = collect(config)
    except (AuditError, ValueError) as error:
        print(
            f"upstream audit failed: {error_detail(str(error), config)}",
            file=sys.stderr,
        )
        return 1
    print(json.dumps(payload, ensure_ascii=True, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
