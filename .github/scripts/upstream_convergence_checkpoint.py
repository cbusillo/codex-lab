#!/usr/bin/env python3
"""Seal and verify committed convergence provenance, without executing repairs."""

import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
from datetime import datetime
from pathlib import Path

import upstream_convergence as driver


SOURCE_ROOT = Path(__file__).resolve().parents[2]
MAX_STATE_BYTES = 1024 * 1024
MAX_CHECKPOINT_BYTES = 16 * 1024 * 1024
SHA256 = re.compile(r"[0-9a-f]{64}")
RAW_ROW = re.compile(
    r":(100644|100755|120000|000000) (100644|100755|120000|000000) "
    r"([0-9a-f]{40}) ([0-9a-f]{40}) ([ADMT])"
)
SOURCE_FILES = (
    ".github/scripts/upstream_convergence_checkpoint.py",
    ".github/scripts/upstream_convergence.py",
    ".github/scripts/upstream_convergence_inventory.py",
    ".github/scripts/upstream_convergence_guard.py",
    ".github/scripts/verify_upstream_convergence_governance.py",
    ".github/scripts/upstream_candidate_preflight.py",
    ".github/scripts/upstream_convergence_repair_ledger.py",
    "upstream/convergence-policy.json",
    "upstream/convergence-contracts.md",
    "upstream/convergence-gates.json",
    "upstream/convergence-guard.json",
    "upstream/convergence-waivers.json",
)


def encoded(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read_json(path: Path, limit: int) -> tuple[dict, str]:
    with path.open("rb") as handle:
        data = handle.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"{path.name} exceeds its byte limit")
    value = json.loads(data)
    if not isinstance(value, dict):
        raise ValueError(f"{path.name} must contain a JSON object")
    return value, digest(data)


def patch_inventory(repo: Path, baseline: str, candidate: str) -> dict:
    """Compare ordered tree tips, retaining every path, blob and file mode."""
    raw = driver.run_git(
        repo,
        "diff",
        "--raw",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--ignore-submodules=none",
        "-O",
        os.devnull,
        "--abbrev=40",
        "-z",
        baseline,
        candidate,
        "--",
    ).stdout
    fields = raw.split("\0")
    if fields.pop() != "" or len(fields) % 2:
        raise ValueError("incomplete raw patch inventory")
    rows = []
    seen = set()
    for header, path in zip(fields[::2], fields[1::2]):
        match = RAW_ROW.fullmatch(header)
        if not match or not path or path in seen or len(path) > 4096:
            raise ValueError("invalid, duplicate or unsupported patch entry")
        old_mode, new_mode, old_blob, new_blob, status = match.groups()
        rows.append(
            {
                "path": path,
                "oldMode": old_mode,
                "newMode": new_mode,
                "oldBlob": old_blob,
                "newBlob": new_blob,
                "status": status,
            }
        )
        seen.add(path)
    return {
        "baseline": baseline,
        "candidate": candidate,
        "pathTotal": len(rows),
        "paths": rows,
    }


def state(args: argparse.Namespace) -> dict:
    repo = driver.repository_root(args.repo)
    source_root = driver.repository_root(SOURCE_ROOT)
    candidate_state = driver.worktree_state(repo)
    source_state = driver.worktree_state(source_root)
    driver.require_record_safety(candidate_state)
    driver.require_read_safety(source_state)
    source = driver.resolve_exact_commit(source_root, args.source, "source")
    if source_state["head"] != source:
        raise ValueError("tooling source HEAD differs from pinned source")
    candidate = driver.resolve_exact_commit(repo, args.candidate, "candidate")
    if candidate_state["head"] != candidate:
        raise ValueError("candidate HEAD differs from pinned candidate")
    refs = driver.exact_refs(repo, args.base, args.upstream, args.local)
    if not all(
        driver.ref_is_ancestor(repo, refs[name], candidate)
        for name in ("local", "upstream")
    ):
        raise ValueError("candidate must contain both pinned local and upstream")
    policy = driver.governance.load_policy(
        source_root / driver.CANONICAL_POLICY_PATH, source_root
    )
    remote = driver.remote_identity(repo, policy, refs["upstream"])
    # Tracking tips can advance while this exact upstream remains reachable.
    remote.pop("remoteTip")
    config, config_digest = read_json(args.config, MAX_STATE_BYTES)
    evidence, evidence_digest = read_json(args.validation, MAX_STATE_BYTES)
    if set(config) != {"schemaVersion", "attemptId", "startedAt", "settings"}:
        raise ValueError("invalid run configuration fields")
    if (
        type(config["schemaVersion"]) is not int
        or config["schemaVersion"] != 1
        or not isinstance(config["attemptId"], str)
        or not re.fullmatch(r"[A-Za-z0-9._-]{1,128}", config["attemptId"])
        or not isinstance(config["settings"], dict)
    ):
        raise ValueError("invalid run configuration")
    started = datetime.fromisoformat(config["startedAt"].replace("Z", "+00:00"))
    if started.tzinfo is None:
        raise ValueError("attempt start must have a timezone")
    if (
        set(evidence) != {"schemaVersion", "attemptId", "candidate", "checks"}
        or type(evidence["schemaVersion"]) is not int
        or evidence["schemaVersion"] != 1
        or evidence["attemptId"] != config["attemptId"]
        or evidence["candidate"] != candidate
        or not isinstance(evidence["checks"], list)
    ):
        raise ValueError("validation evidence must name this attempt and candidate")
    for check in evidence["checks"]:
        if (
            not isinstance(check, dict)
            or set(check) != {"name", "outcome", "artifactSha256"}
            or not isinstance(check["name"], str)
            or not check["name"]
            or check["outcome"] not in {"passed", "failed", "not-run"}
            or (
                check["artifactSha256"] is not None
                and not SHA256.fullmatch(str(check["artifactSha256"]))
            )
        ):
            raise ValueError("invalid observed validation entry")
    blobs = {}
    for path in SOURCE_FILES:
        data = (source_root / path).read_bytes()
        blob = driver.run_git(
            source_root, "rev-parse", f"{source}:{path}"
        ).stdout.strip()
        if driver.guard.blob_id(data) != blob:
            raise ValueError(f"tooling file differs from committed source: {path}")
        blobs[path] = blob
    upstream_patch = patch_inventory(repo, refs["upstream"], candidate)
    local_patch = patch_inventory(repo, refs["local"], candidate)
    result = {
        "schemaVersion": 1,
        "kind": "committed-convergence-provenance",
        "refs": {**refs, "candidate": candidate, "source": source},
        "worktree": {
            "root": str(repo),
            "commonDir": str(driver.git_common_dir(repo)),
            "branch": candidate_state["branch"],
        },
        "tooling": {"root": str(source_root), "blobs": blobs},
        "upstream": remote,
        "attemptId": config["attemptId"],
        "startedAt": config["startedAt"],
        "configurationSha256": config_digest,
        "validationSha256": evidence_digest,
        "patch": upstream_patch,
        "localPatch": {
            "baseline": refs["local"],
            "candidate": candidate,
            "pathTotal": local_patch["pathTotal"],
            "sha256": digest(encoded(local_patch)),
        },
    }
    if len(encoded(result)) > MAX_CHECKPOINT_BYTES:
        raise ValueError(
            "complete checkpoint exceeds byte limit; no truncated checkpoint"
        )
    # A concurrent mutation must not receive a successful checkpoint receipt.
    if candidate_state != driver.worktree_state(
        repo
    ) or source_state != driver.worktree_state(source_root):
        raise ValueError("worktree changed while checking provenance")
    return result


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("record", "verify"))
    parser.add_argument("--repo", type=Path, required=True)
    for name in ("base", "local", "upstream", "candidate", "source"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--validation", type=Path, required=True)
    parser.add_argument("--checkpoint", type=Path, required=True)
    parser.add_argument("--sha256")
    args = parser.parse_args(argv)
    temporary = None
    recorded = None
    try:
        args.repo = driver.repository_root(args.repo)
        checkpoint = args.checkpoint.resolve()
        for root in (args.repo.resolve(), SOURCE_ROOT):
            if any(
                path.resolve().is_relative_to(root)
                for path in (checkpoint, args.config, args.validation)
            ):
                raise ValueError("checkpoint and inputs must be outside both worktrees")
        if args.operation == "verify":
            if not SHA256.fullmatch(args.sha256 or ""):
                raise ValueError(
                    "verify requires the independently retained checkpoint SHA256"
                )
            recorded, sha = read_json(checkpoint, MAX_CHECKPOINT_BYTES)
            if sha != args.sha256:
                raise ValueError("checkpoint digest differs from retained receipt")
        with driver.convergence_lock(args.repo):
            expected = state(args)
            data = encoded(expected)
            sha = digest(data)
            if args.operation == "verify":
                if recorded != expected:
                    raise ValueError(
                        "checkpoint provenance or complete ordered inventory changed"
                    )
            else:
                with tempfile.NamedTemporaryFile(
                    dir=checkpoint.parent, delete=False
                ) as handle:
                    temporary = Path(handle.name)
                    handle.write(data)
                    handle.flush()
                    os.fsync(handle.fileno())
                os.link(
                    temporary, checkpoint
                )  # Atomic publication without replacement.
        print(
            json.dumps(
                {
                    "status": "recorded" if args.operation == "record" else "unchanged",
                    "candidate": expected["refs"]["candidate"],
                    "attemptId": expected["attemptId"],
                    "pathTotal": expected["patch"]["pathTotal"],
                    "checkpoint": str(checkpoint),
                    "sha256": sha,
                    "validation": "provenance only; no checks executed",
                }
            )
        )
        return 0
    except (
        OSError,
        ValueError,
        TypeError,
        KeyError,
        AttributeError,
        driver.ConvergenceError,
    ) as error:
        print(f"checkpoint refused: {str(error)[:512]}", file=sys.stderr)
        return 1
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
