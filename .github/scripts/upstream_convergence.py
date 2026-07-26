#!/usr/bin/env python3

"""Inspect, record, and validate an upstream convergence snapshot safely."""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from contextlib import contextmanager
from pathlib import Path
from urllib.parse import urlparse

import upstream_convergence_guard as guard
import upstream_convergence_inventory as inventory
import verify_upstream_convergence_governance as governance


REPO_ROOT = Path(__file__).resolve().parents[2]
POLICY_PATH = REPO_ROOT / "upstream" / "convergence-policy.json"
FULL_SHA = re.compile(r"[0-9a-f]{40}")
SNAPSHOT_FILES = ("inventory.json", "inventory.md", "residuals.json")


class ConvergenceError(RuntimeError):
    """The requested operation cannot produce trustworthy evidence."""


def run_git(
    repo: Path, *args: str, check: bool = True
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
    )
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise ConvergenceError(f"git {' '.join(args)} failed: {detail}")
    return result


def resolve_exact_commit(repo: Path, value: str, name: str) -> str:
    if FULL_SHA.fullmatch(value) is None:
        raise ConvergenceError(f"{name} must be a full 40-character commit SHA")
    resolved = run_git(repo, "rev-parse", f"{value}^{{commit}}").stdout.strip()
    if resolved != value:
        raise ConvergenceError(f"{name} resolved to {resolved}, expected {value}")
    return resolved


def repository_root(repo: Path) -> Path:
    root = Path(run_git(repo, "rev-parse", "--show-toplevel").stdout.strip()).resolve()
    if root != repo.resolve():
        raise ConvergenceError(f"repo root is {root}, not {repo.resolve()}")
    return root


def git_common_dir(repo: Path) -> Path:
    raw = Path(run_git(repo, "rev-parse", "--git-common-dir").stdout.strip())
    return raw.resolve() if raw.is_absolute() else (repo / raw).resolve()


@contextmanager
def convergence_lock(repo: Path):
    lock_path = git_common_dir(repo) / "upstream-convergence.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    handle = lock_path.open("a+", encoding="utf-8")
    try:
        try:
            import fcntl

            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except ImportError:
            import msvcrt

            msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
        except OSError as error:
            raise ConvergenceError(
                f"another upstream convergence operation holds {lock_path}"
            ) from error
        handle.seek(0)
        handle.truncate()
        handle.write(f"pid={os.getpid()} worktree={repo.resolve()}\n")
        handle.flush()
        yield
    finally:
        try:
            if "fcntl" in sys.modules:
                sys.modules["fcntl"].flock(handle.fileno(), sys.modules["fcntl"].LOCK_UN)
            elif "msvcrt" in sys.modules:
                handle.seek(0)
                sys.modules["msvcrt"].locking(
                    handle.fileno(), sys.modules["msvcrt"].LK_UNLCK, 1
                )
        finally:
            handle.close()


def operation_markers(repo: Path) -> list[str]:
    markers = []
    for name in (
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-apply",
        "rebase-merge",
    ):
        raw = Path(run_git(repo, "rev-parse", "--git-path", name).stdout.strip())
        path = raw if raw.is_absolute() else repo / raw
        if path.exists():
            markers.append(name)
    return markers


def default_branch(repo: Path) -> str:
    result = run_git(
        repo,
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
        check=False,
    )
    if result.returncode == 0 and result.stdout.strip():
        return result.stdout.strip().removeprefix("origin/")
    return "main"


def worktree_state(repo: Path) -> dict[str, object]:
    branch_result = run_git(
        repo, "symbolic-ref", "--quiet", "--short", "HEAD", check=False
    )
    branch = branch_result.stdout.strip() if branch_result.returncode == 0 else None
    status = run_git(repo, "status", "--porcelain=v1", "--untracked-files=all").stdout
    worktrees = [
        line.removeprefix("worktree ")
        for line in run_git(repo, "worktree", "list", "--porcelain").stdout.splitlines()
        if line.startswith("worktree ")
    ]
    return {
        "root": str(repo.resolve()),
        "head": run_git(repo, "rev-parse", "HEAD").stdout.strip(),
        "branch": branch,
        "defaultBranch": default_branch(repo),
        "clean": not status,
        "operationMarkers": operation_markers(repo),
        "primaryWorktree": str(Path(worktrees[0]).resolve()) if worktrees else None,
    }


def require_read_safety(state: dict[str, object]) -> None:
    if not state["clean"]:
        raise ConvergenceError("worktree must be clean before inspecting exact refs")
    if state["operationMarkers"]:
        markers = ", ".join(state["operationMarkers"])
        raise ConvergenceError(f"Git operation is already in progress: {markers}")


def require_record_safety(state: dict[str, object]) -> None:
    require_read_safety(state)
    branch = state["branch"]
    if branch is None:
        raise ConvergenceError("record requires an attached task branch")
    if branch in {state["defaultBranch"], "main", "master"}:
        raise ConvergenceError(f"record refuses protected/default branch {branch}")
    if state["root"] == state["primaryWorktree"]:
        raise ConvergenceError("record requires an isolated linked worktree")


def normalize_remote_url(value: str) -> str:
    raw = value.strip().removesuffix("/").removesuffix(".git")
    if raw.startswith("git@") and ":" in raw:
        host, path = raw.removeprefix("git@").split(":", 1)
        return f"{host.lower()}/{path.strip('/')}"
    parsed = urlparse(raw)
    if parsed.scheme in {"http", "https", "ssh", "git"} and parsed.hostname:
        path = parsed.path.strip("/")
        return f"{parsed.hostname.lower()}/{path}"
    raise ConvergenceError(f"unsupported upstream fetch URL: {value}")


def remote_identity(
    repo: Path,
    policy: governance.ConvergencePolicy,
    upstream: str | None = None,
    *,
    require_remote_ref: bool = True,
) -> dict[str, object]:
    fetch_url = run_git(repo, "remote", "get-url", policy.remote).stdout.strip()
    normalized = normalize_remote_url(fetch_url)
    allowed = {normalize_remote_url(value) for value in policy.allowed_fetch_urls}
    if normalized not in allowed:
        raise ConvergenceError(
            f"remote {policy.remote} fetch URL is {fetch_url}, expected {sorted(allowed)}"
        )
    remote_ref = f"refs/remotes/{policy.remote}/{policy.branch}"
    tip_result = run_git(
        repo, "rev-parse", f"{remote_ref}^{{commit}}", check=require_remote_ref
    )
    remote_tip = tip_result.stdout.strip() if tip_result.returncode == 0 else None
    if upstream is not None:
        if remote_tip is None:
            raise ConvergenceError(f"missing upstream tracking ref {remote_ref}")
        result = run_git(
            repo, "merge-base", "--is-ancestor", upstream, remote_tip, check=False
        )
        if result.returncode != 0:
            raise ConvergenceError(
                f"upstream {upstream} is not reachable from {remote_ref} at {remote_tip}"
            )
    return {
        "repository": policy.repository,
        "remote": policy.remote,
        "fetchUrl": fetch_url,
        "remoteRef": remote_ref,
        "remoteTip": remote_tip,
        "provenance": "local remote-tracking ref with canonical fetch URL",
    }


def optional_remote_identity(
    repo: Path, policy: governance.ConvergencePolicy
) -> dict[str, object]:
    exists = run_git(repo, "remote", "get-url", policy.remote, check=False)
    if exists.returncode != 0:
        return {
            "repository": policy.repository,
            "remote": policy.remote,
            "configured": False,
            "provenance": "not available in this validation checkout",
        }
    return {
        **remote_identity(repo, policy, require_remote_ref=False),
        "configured": True,
    }


def exact_refs(
    repo: Path, base: str, upstream: str, local: str
) -> dict[str, str]:
    resolved = {
        "base": resolve_exact_commit(repo, base, "base"),
        "upstream": resolve_exact_commit(repo, upstream, "upstream"),
        "local": resolve_exact_commit(repo, local, "local"),
    }
    actual_base = run_git(
        repo, "merge-base", resolved["upstream"], resolved["local"]
    ).stdout.strip()
    if actual_base != resolved["base"]:
        raise ConvergenceError(
            f"expected merge base {resolved['base']}, found {actual_base}"
        )
    return resolved


def governance_changes(repo: Path, base: str, upstream: str) -> list[dict[str, object]]:
    paths = inventory.changed_paths(repo, base, upstream)
    return [
        classified
        for path in sorted(paths)
        if "GOVERNANCE-1" in (classified := inventory.classify_path(path))["contracts"]
    ]


def recorded_upstream_tip(
    repo: Path, policy: governance.ConvergencePolicy
) -> str | None:
    commits = []
    for snapshot in snapshot_directories(repo, policy.evidence_root):
        if snapshot.name.startswith("."):
            continue
        try:
            document = json.loads(
                (snapshot / "inventory.json").read_text(encoding="utf-8")
            )
            upstream = str(document["refs"]["upstream"])
        except (KeyError, json.JSONDecodeError, OSError, TypeError) as error:
            raise ConvergenceError(f"cannot read upstream ref from {snapshot}: {error}")
        if FULL_SHA.fullmatch(upstream) is None or not commit_exists(repo, upstream):
            raise ConvergenceError(
                f"recorded upstream {upstream!r} from {snapshot} is unavailable"
            )
        commits.append(upstream)
    if not commits:
        return None
    tips = [
        commit
        for commit in commits
        if not any(
            commit != other and ref_is_ancestor(repo, commit, other)
            for other in commits
        )
    ]
    if len(set(tips)) != 1:
        raise ConvergenceError(f"recorded upstream history has multiple tips: {tips}")
    return tips[0]


def require_forward_upstream(
    repo: Path, policy: governance.ConvergencePolicy, upstream: str
) -> str | None:
    previous = recorded_upstream_tip(repo, policy)
    if previous is not None and not ref_is_ancestor(repo, previous, upstream):
        raise ConvergenceError(
            f"upstream moved backward or diverged: {previous} is not an ancestor of {upstream}"
        )
    return previous


def inspect(
    repo: Path,
    policy: governance.ConvergencePolicy,
    base: str,
    upstream: str,
    local: str,
) -> dict[str, object]:
    state = worktree_state(repo)
    require_read_safety(state)
    refs = exact_refs(repo, base, upstream, local)
    remote = remote_identity(repo, policy, refs["upstream"])
    previous_upstream = require_forward_upstream(repo, policy, refs["upstream"])
    result = inventory.build_inventory(
        repo,
        refs["base"],
        refs["upstream"],
        refs["local"],
    )
    return {
        "schemaVersion": 1,
        "operation": "inspect",
        "passed": True,
        "worktree": state,
        "upstream": remote,
        "refs": refs,
        "previousRecordedUpstream": previous_upstream,
        "inventory": {
            "policyVersion": inventory.POLICY_VERSION,
            "summary": result["summary"],
            "conflictTypeCounts": result["conflictTypeCounts"],
            "laneCounts": result["laneCounts"],
            "residualLaneCounts": result["residualLaneCounts"],
        },
        "governanceChanges": governance_changes(
            repo, refs["base"], refs["upstream"]
        ),
        "nextAction": "Review non-green paths before applying the upstream merge.",
    }


def ref_is_ancestor(repo: Path, ancestor: str, descendant: str) -> bool:
    return (
        run_git(
            repo, "merge-base", "--is-ancestor", ancestor, descendant, check=False
        ).returncode
        == 0
    )


def record(
    repo: Path,
    policy: governance.ConvergencePolicy,
    base: str,
    upstream: str,
    local: str,
) -> dict[str, object]:
    state = worktree_state(repo)
    require_record_safety(state)
    refs = exact_refs(repo, base, upstream, local)
    remote = remote_identity(repo, policy, refs["upstream"])
    previous_upstream = require_forward_upstream(repo, policy, refs["upstream"])
    head = str(state["head"])
    if head != refs["local"] and not (
        ref_is_ancestor(repo, refs["local"], head)
        and ref_is_ancestor(repo, refs["upstream"], head)
    ):
        raise ConvergenceError(
            "HEAD must equal the local input or contain both local and upstream inputs"
        )

    result = inventory.build_inventory(
        repo,
        refs["base"],
        refs["upstream"],
        refs["local"],
    )
    snapshot_id = f"{refs['base'][:8]}-{refs['upstream'][:8]}"
    evidence_root = repo / policy.evidence_root
    evidence_root.mkdir(parents=True, exist_ok=True)
    free_bytes = shutil.disk_usage(evidence_root).free
    if free_bytes < 64 * 1024 * 1024:
        raise ConvergenceError(
            f"insufficient free space under {evidence_root}: {free_bytes} bytes"
        )
    destination = evidence_root / snapshot_id
    if destination.exists():
        raise ConvergenceError(f"snapshot already exists: {destination}")

    temporary = Path(tempfile.mkdtemp(prefix=f".{snapshot_id}.", dir=evidence_root))
    try:
        (temporary / "inventory.json").write_text(
            inventory.render_json(result), encoding="utf-8"
        )
        (temporary / "inventory.md").write_text(
            inventory.render_markdown(result), encoding="utf-8"
        )
        (temporary / "residuals.json").write_text(
            inventory.render_residuals(result), encoding="utf-8"
        )
        if run_git(repo, "rev-parse", "HEAD").stdout.strip() != head:
            raise ConvergenceError("HEAD changed while the snapshot was being recorded")
        temporary.rename(destination)
    except BaseException:
        shutil.rmtree(temporary, ignore_errors=True)
        raise

    return {
        "schemaVersion": 1,
        "operation": "record",
        "passed": True,
        "worktree": state,
        "upstream": remote,
        "refs": refs,
        "previousRecordedUpstream": previous_upstream,
        "snapshot": str(destination.relative_to(repo)),
        "freeBytesBeforeRecord": free_bytes,
        "summary": result["summary"],
        "nextAction": "Review and commit the new snapshot before merging or publishing.",
    }


def snapshot_directories(repo: Path, evidence_root: str) -> list[Path]:
    root = repo / evidence_root
    if not root.is_dir():
        return []
    return sorted(path for path in root.iterdir() if path.is_dir())


def commit_exists(repo: Path, commit: str) -> bool:
    return run_git(repo, "cat-file", "-e", f"{commit}^{{commit}}", check=False).returncode == 0


def validate_snapshots(
    repo: Path, policy: governance.ConvergencePolicy
) -> dict[str, object]:
    errors: list[str] = []
    reproduced: list[str] = []
    unavailable: list[str] = []
    directories = snapshot_directories(repo, policy.evidence_root)
    if not directories:
        errors.append(f"no snapshots found under {policy.evidence_root}")

    for snapshot in directories:
        relative = str(snapshot.relative_to(repo))
        if snapshot.name.startswith("."):
            errors.append(f"incomplete temporary snapshot remains: {relative}")
            continue
        files = sorted(path.name for path in snapshot.iterdir() if path.is_file())
        if files != sorted(SNAPSHOT_FILES):
            errors.append(
                f"{relative} contains {files}, expected {sorted(SNAPSHOT_FILES)}"
            )
            continue
        try:
            document = json.loads((snapshot / "inventory.json").read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError) as error:
            errors.append(f"cannot read {relative}/inventory.json: {error}")
            continue
        refs = document.get("refs")
        if not isinstance(refs, dict) or any(
            FULL_SHA.fullmatch(str(refs.get(name, ""))) is None
            for name in ("base", "upstream", "local")
        ):
            errors.append(f"{relative} does not contain exact base/upstream/local refs")
            continue
        expected_name = f"{refs['base'][:8]}-{refs['upstream'][:8]}"
        if snapshot.name != expected_name:
            errors.append(f"{relative} should be named {expected_name}")
        if document.get("repository") != policy.repository:
            errors.append(f"{relative} records unexpected repository identity")
        raw_policy = document.get("policy")
        policy_version = (
            raw_policy.get("version", inventory.LEGACY_POLICY_VERSION)
            if isinstance(raw_policy, dict)
            else inventory.LEGACY_POLICY_VERSION
        )
        if policy_version not in inventory.SUPPORTED_POLICY_VERSIONS:
            errors.append(f"{relative} uses unsupported policy version {policy_version}")
            continue
        if not all(commit_exists(repo, str(refs[name])) for name in refs):
            unavailable.append(relative)
            continue
        try:
            generated = inventory.build_inventory(
                repo,
                str(refs["base"]),
                str(refs["upstream"]),
                str(refs["local"]),
                int(policy_version),
            )
        except (RuntimeError, ValueError, subprocess.SubprocessError) as error:
            errors.append(f"cannot reproduce {relative}: {error}")
            continue
        expected = {
            "inventory.json": inventory.render_json(generated),
            "inventory.md": inventory.render_markdown(generated),
            "residuals.json": inventory.render_residuals(generated),
        }
        mismatched = [
            name
            for name, contents in expected.items()
            if (snapshot / name).read_text(encoding="utf-8") != contents
        ]
        if mismatched:
            errors.append(f"{relative} is not reproducible: {mismatched}")
        else:
            reproduced.append(relative)

    return {
        "count": len(directories),
        "reproduced": reproduced,
        "historyUnavailable": unavailable,
        "errors": errors,
        "passed": not errors,
    }


def snapshot_change_errors(
    repo: Path, policy: governance.ConvergencePolicy, against: str
) -> list[str]:
    base = resolve_exact_commit(repo, against, "against")
    tree = run_git(
        repo,
        "ls-tree",
        "-d",
        "--name-only",
        f"{base}:{policy.evidence_root}",
        check=False,
    )
    existing = set(tree.stdout.splitlines()) if tree.returncode == 0 else set()
    changes = run_git(
        repo,
        "diff",
        "--name-status",
        "--no-renames",
        f"{base}..HEAD",
        "--",
        policy.evidence_root,
    ).stdout.splitlines()
    errors = []
    prefix = f"{policy.evidence_root}/"
    for line in changes:
        status, path = line.split("\t", 1)
        if not path.startswith(prefix):
            continue
        snapshot = path.removeprefix(prefix).split("/", 1)[0]
        if snapshot in existing:
            errors.append(f"historical snapshot changed ({status}): {path}")
        elif status != "A":
            errors.append(f"new snapshot path is not an addition ({status}): {path}")
    return errors


def validate(
    repo: Path,
    policy: governance.ConvergencePolicy,
    policy_path: Path,
    against: str | None,
) -> dict[str, object]:
    state = worktree_state(repo)
    remote = optional_remote_identity(repo, policy)
    governance_report = governance.verify(repo, policy_path)
    manifest = guard.load_manifest(repo / "upstream" / "convergence-guard.json")
    waivers = guard.load_waivers(repo / "upstream" / "convergence-waivers.json")
    guard_report = guard.check(manifest, waivers, repo)
    snapshots = validate_snapshots(repo, policy)
    errors = []
    if against is not None:
        errors.extend(snapshot_change_errors(repo, policy, against))
    if state["operationMarkers"]:
        errors.append(
            "Git operation is in progress: " + ", ".join(state["operationMarkers"])
        )
    if not state["clean"]:
        errors.append("worktree must be clean for exact validation evidence")
    waiver_dispositions: dict[str, int] = {}
    for entry in guard_report["waived"]:
        disposition = str(entry["disposition"])
        waiver_dispositions[disposition] = waiver_dispositions.get(disposition, 0) + 1
    guard_summary = {
        "guardedPaths": guard_report["guardedPaths"],
        "passed": guard_report["passed"],
        "violationCount": len(guard_report["violations"]),
        "waivedCount": len(guard_report["waived"]),
        "staleWaiverCount": len(guard_report["staleWaivers"]),
        "waiverDispositionCounts": dict(sorted(waiver_dispositions.items())),
        "violations": guard_report["violations"],
        "staleWaivers": guard_report["staleWaivers"],
    }
    passed = (
        governance_report["passed"]
        and guard_report["passed"]
        and snapshots["passed"]
        and not errors
    )
    return {
        "schemaVersion": 1,
        "operation": "validate",
        "passed": passed,
        "worktree": state,
        "upstream": remote,
        "governance": governance_report,
        "guard": guard_summary,
        "snapshots": snapshots,
        "comparisonBase": against,
        "errors": errors,
        "nextAction": (
            "Convergence controls are valid."
            if passed
            else "Resolve reported convergence control failures before integration."
        ),
    }


def print_report(report: dict[str, object]) -> None:
    operation = report["operation"]
    if operation == "inspect":
        summary = report["inventory"]["summary"]
        print(
            f"Inspect passed: {summary['conflicts']} conflicts, "
            f"{summary['residualLocalInfluence']} residual paths."
        )
    elif operation == "record":
        print(f"Recorded upstream convergence snapshot at {report['snapshot']}.")
    else:
        snapshots = report["snapshots"]
        print(
            f"Validate {'passed' if report['passed'] else 'failed'}: "
            f"{snapshots['count']} snapshots, "
            f"{len(snapshots['reproduced'])} reproduced, "
            f"{len(snapshots['historyUnavailable'])} without local history."
        )
    print(report["nextAction"])


def add_common_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--repo-root", default=str(REPO_ROOT))
    parser.add_argument("--policy", default=str(POLICY_PATH))
    parser.add_argument("--json", action="store_true")


def add_ref_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--base", required=True)
    parser.add_argument("--upstream", required=True)
    parser.add_argument("--local", required=True)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="operation", required=True)

    inspect_parser = subparsers.add_parser("inspect")
    add_common_arguments(inspect_parser)
    add_ref_arguments(inspect_parser)

    record_parser = subparsers.add_parser("record")
    add_common_arguments(record_parser)
    add_ref_arguments(record_parser)

    validate_parser = subparsers.add_parser("validate")
    add_common_arguments(validate_parser)
    validate_parser.add_argument("--against")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    repo = Path(args.repo_root).resolve()
    policy_path = Path(args.policy).resolve()
    try:
        repository_root(repo)
        policy = governance.load_policy(policy_path, repo)
        with convergence_lock(repo):
            if args.operation == "inspect":
                report = inspect(repo, policy, args.base, args.upstream, args.local)
            elif args.operation == "record":
                report = record(repo, policy, args.base, args.upstream, args.local)
            else:
                report = validate(repo, policy, policy_path, args.against)
    except (ConvergenceError, governance.PolicyError, guard.WaiverError) as error:
        if args.json:
            json.dump(
                {
                    "schemaVersion": 1,
                    "operation": args.operation,
                    "passed": False,
                    "errors": [str(error)],
                },
                sys.stdout,
                indent=2,
                sort_keys=True,
            )
            print()
        else:
            print(f"upstream convergence {args.operation} failed: {error}", file=sys.stderr)
        return 2

    if args.json:
        json.dump(report, sys.stdout, indent=2, sort_keys=True)
        print()
    else:
        print_report(report)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
