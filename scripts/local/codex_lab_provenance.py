#!/usr/bin/env python3
"""Verify and stage immutable Codex Lab dogfood binaries."""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

try:
    import fcntl
except ImportError:
    fcntl = None

REPORT_SCHEMA_VERSION = 1
SUPPORTED_PROVENANCE_SCHEMA_VERSIONS = {1, 2}
COMMAND_TIMEOUT_SECONDS = 30
COMMIT_LENGTHS = {40, 64}
DIRTY_STATES = {"clean", "dirty"}
MAX_STAGED_CANDIDATES = 8
SAFE_COMPONENT = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]*\Z")


class ProvenanceError(Exception):
    """Raised when checkout or binary provenance cannot be read."""


def run_text(command: list[str], description: str) -> str:
    try:
        result = subprocess.run(
            command,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=COMMAND_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        raise ProvenanceError(f"{description} timed out") from error
    except OSError as error:
        raise ProvenanceError(f"could not run {description}: {error}") from error
    if result.returncode != 0:
        detail = (
            result.stderr.strip() or result.stdout.strip() or "no diagnostic output"
        )
        raise ProvenanceError(
            f"{description} failed (exit {result.returncode}): {detail}"
        )
    return result.stdout.strip()


def is_commit(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) in COMMIT_LENGTHS
        and all(character in "0123456789abcdefABCDEF" for character in value)
    )


def checkout_identity(repo_root: Path) -> dict[str, str]:
    repo_root = repo_root.expanduser().resolve(strict=True)
    top_level = Path(
        run_text(
            ["git", "-C", str(repo_root), "rev-parse", "--show-toplevel"],
            f"Git root from {repo_root}",
        )
    ).resolve(strict=True)
    if top_level != repo_root:
        raise ProvenanceError(f"expected Git root {repo_root}, found {top_level}")
    source_commit = run_text(
        ["git", "-C", str(repo_root), "rev-parse", "--verify", "HEAD"],
        f"Git commit from {repo_root}",
    )
    if not is_commit(source_commit):
        raise ProvenanceError(f"checkout {repo_root} did not report an exact commit")
    status = run_text(
        [
            "git",
            "-C",
            str(repo_root),
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
        f"Git status from {repo_root}",
    )
    return {
        "source_commit": source_commit.lower(),
        "dirty_state": "dirty" if status else "clean",
    }


def reported_provenance(binary_path: Path) -> dict[str, Any]:
    output = run_text(
        [str(binary_path), "debug", "provenance", "--json"],
        f"build provenance from {binary_path}",
    )
    try:
        provenance = json.loads(output)
    except json.JSONDecodeError as error:
        raise ProvenanceError(
            f"{binary_path} emitted invalid provenance JSON: {error}"
        ) from error
    if not isinstance(provenance, dict):
        raise ProvenanceError(f"{binary_path} emitted a non-object provenance record")
    return provenance


def unavailable_report(
    binary_path: Path, expected: dict[str, str] | None, failure: str
) -> dict[str, Any]:
    try:
        display_path = str(binary_path.expanduser().resolve(strict=False))
    except (OSError, RuntimeError, ValueError):
        display_path = str(binary_path)
    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "status": "unverifiable",
        "binary_path": display_path,
        "expected": expected,
        "provenance": None,
        "failures": [failure],
    }


def verification_report(
    expected: dict[str, str], binary_path: Path, provenance: dict[str, Any]
) -> dict[str, Any]:
    binary_path = binary_path.expanduser().resolve(strict=True)
    invalid: list[str] = []
    mismatches: list[str] = []
    source_commit = provenance.get("source_commit")
    dirty_state = provenance.get("dirty_state")
    executable_path = provenance.get("executable_path")

    provenance_schema_version = provenance.get("schema_version")
    if (
        type(provenance_schema_version) is not int
        or provenance_schema_version not in SUPPORTED_PROVENANCE_SCHEMA_VERSIONS
    ):
        invalid.append("binary did not report the supported provenance schema")
    if not isinstance(provenance.get("version"), str) or not provenance["version"]:
        invalid.append("binary version is unavailable")
    if provenance_schema_version == 2:
        release_version = provenance.get("release_version")
        compatibility_version = provenance.get("compatibility_version")
        if not isinstance(release_version, str) or not release_version:
            invalid.append("binary release version is unavailable")
        if not isinstance(compatibility_version, str) or not compatibility_version:
            invalid.append("binary compatibility version is unavailable")
        elif provenance.get("version") != compatibility_version:
            invalid.append("binary compatibility version aliases disagree")
    if not is_commit(source_commit):
        invalid.append("binary source commit is unavailable")
    elif source_commit.lower() != expected["source_commit"]:
        mismatches.append(
            f"binary commit {source_commit.lower()} does not match expected {expected['source_commit']}"
        )
    if not isinstance(dirty_state, str) or dirty_state not in DIRTY_STATES:
        invalid.append("binary dirty state is unavailable")
    elif dirty_state != expected["dirty_state"]:
        mismatches.append(
            f"binary dirty state {dirty_state} does not match expected {expected['dirty_state']}"
        )
    for field in ("build_profile", "build_channel"):
        value = provenance.get(field)
        if (
            not isinstance(value, str)
            or value == "unavailable"
            or SAFE_COMPONENT.fullmatch(value) is None
        ):
            invalid.append(f"binary {field.replace('_', ' ')} is unavailable or unsafe")
    if not isinstance(executable_path, str) or executable_path == "unavailable":
        invalid.append("binary executable path is unavailable")
    else:
        try:
            reported_path = Path(executable_path).expanduser().resolve(strict=False)
        except (OSError, RuntimeError, ValueError):
            invalid.append("binary executable path is malformed")
        else:
            if reported_path != binary_path:
                invalid.append(
                    f"binary reported executable {executable_path}, expected {binary_path}"
                )

    status = "unverifiable" if invalid else "stale" if mismatches else "current"
    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "status": status,
        "binary_path": str(binary_path),
        "expected": expected,
        "provenance": provenance,
        "failures": invalid + mismatches,
    }


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def normalize_companion_sources(paths: list[Path]) -> dict[str, Path]:
    companions: dict[str, Path] = {}
    for path in paths:
        name = path.name
        if name == "codex-lab" or SAFE_COMPONENT.fullmatch(name) is None:
            raise ProvenanceError(f"unsafe companion binary name: {name}")
        if name in companions:
            raise ProvenanceError(f"duplicate companion binary name: {name}")
        source = path.expanduser().resolve(strict=True)
        if not source.is_file():
            raise ProvenanceError(f"companion binary is not a file: {source}")
        if not source.stat().st_mode & 0o111:
            raise ProvenanceError(f"companion binary is not executable: {source}")
        companions[name] = source
    return companions


def candidate_bundle_digest(
    binary_digest: str, companion_digests: dict[str, str]
) -> str:
    if not companion_digests:
        return binary_digest
    digest = hashlib.sha256()
    members = {"codex-lab": binary_digest, **companion_digests}
    for name, member_digest in sorted(members.items()):
        digest.update(name.encode("utf-8"))
        digest.update(b"\0")
        digest.update(member_digest.encode("ascii"))
        digest.update(b"\n")
    return digest.hexdigest()


def verify_binary(repo_root: Path, binary_path: Path) -> dict[str, Any]:
    expected = checkout_identity(repo_root)
    binary_path = binary_path.expanduser().resolve(strict=True)
    digest = sha256_file(binary_path)
    if checkout_identity(repo_root) != expected:
        return unavailable_report(
            binary_path, expected, "checkout changed during verification"
        )
    report = verification_report(
        expected, binary_path, reported_provenance(binary_path)
    )
    if sha256_file(binary_path) != digest:
        return unavailable_report(
            binary_path, expected, "binary changed during verification"
        )
    if checkout_identity(repo_root) != expected:
        return unavailable_report(
            binary_path, expected, "checkout changed during verification"
        )
    report["binary_sha256"] = digest
    return report


def ensure_private_directory(path: Path) -> None:
    path.mkdir(mode=0o700, exist_ok=True)
    if path.is_symlink() or path.resolve(strict=True) != path:
        raise ProvenanceError(f"candidate directory must not be a symlink: {path}")
    path.chmod(0o700)


def prepare_private_root(path: Path) -> None:
    missing: list[Path] = []
    ancestor = path
    while not ancestor.exists():
        missing.append(ancestor)
        ancestor = ancestor.parent
    effective_uid = os.geteuid()
    metadata = ancestor.stat()
    private_owner = metadata.st_uid == effective_uid and not metadata.st_mode & 0o022
    root_sticky = metadata.st_uid == 0 and metadata.st_mode & 0o1000
    if not private_owner and not root_sticky:
        raise ProvenanceError(f"candidate ancestor is not owner-controlled: {ancestor}")
    child = ancestor
    while child.parent != child:
        parent = child.parent
        metadata = parent.stat()
        if metadata.st_uid not in (0, effective_uid):
            raise ProvenanceError(f"candidate parent has an untrusted owner: {parent}")
        if metadata.st_mode & 0o022 and (
            not metadata.st_mode & 0o1000 or child.stat().st_uid != effective_uid
        ):
            raise ProvenanceError(f"candidate parent is replaceable: {parent}")
        child = parent
    for directory in reversed(missing):
        directory.mkdir(mode=0o700)
        ensure_private_directory(directory)


def verify_published_candidate(
    repo_root: Path,
    expected: dict[str, str],
    binary_path: Path,
    binary_digest: str,
    bundle_digest: str,
    companion_digests: dict[str, str],
) -> dict[str, Any]:
    final_dir = binary_path.parent
    if final_dir.name != bundle_digest:
        return unavailable_report(
            binary_path,
            expected,
            "immutable candidate path does not match its bundle digest",
        )
    if final_dir.stat().st_mode & 0o222:
        return unavailable_report(
            binary_path, expected, "immutable candidate directory is still writable"
        )
    if final_dir.is_symlink() or binary_path.is_symlink() or not binary_path.is_file():
        return unavailable_report(
            binary_path, expected, "immutable candidate is missing or is a symlink"
        )
    if binary_path.stat().st_mode & 0o222:
        return unavailable_report(
            binary_path, expected, "immutable candidate binary is still writable"
        )
    if sha256_file(binary_path) != binary_digest:
        return unavailable_report(
            binary_path, expected, "immutable candidate digest does not match its path"
        )
    for name, expected_digest in companion_digests.items():
        companion = final_dir / name
        if companion.is_symlink() or not companion.is_file():
            return unavailable_report(
                binary_path,
                expected,
                f"immutable candidate companion is missing or is a symlink: {name}",
            )
        if companion.stat().st_mode & 0o222:
            return unavailable_report(
                binary_path,
                expected,
                f"immutable candidate companion is still writable: {name}",
            )
        if not companion.stat().st_mode & 0o111:
            return unavailable_report(
                binary_path,
                expected,
                f"immutable candidate companion is not executable: {name}",
            )
        if sha256_file(companion) != expected_digest:
            return unavailable_report(
                binary_path,
                expected,
                f"immutable candidate companion digest does not match: {name}",
            )
    report = verification_report(
        expected, binary_path, reported_provenance(binary_path)
    )
    if checkout_identity(repo_root) != expected:
        return unavailable_report(
            binary_path, expected, "checkout changed during staging"
        )
    report["binary_sha256"] = binary_digest
    report["bundle_sha256"] = bundle_digest
    if companion_digests:
        report["companion_sha256"] = dict(sorted(companion_digests.items()))
    return report


def staged_candidate_directories(candidate_root: Path) -> list[Path]:
    candidates: list[Path] = []
    for commit_root in candidate_root.iterdir():
        if (
            commit_root.is_symlink()
            or not commit_root.is_dir()
            or not is_commit(commit_root.name)
        ):
            continue
        for state_root in commit_root.iterdir():
            if (
                state_root.is_symlink()
                or not state_root.is_dir()
                or state_root.name not in DIRTY_STATES
            ):
                continue
            for candidate in state_root.iterdir():
                binary = candidate / "codex-lab"
                if (
                    candidate.is_symlink()
                    or not candidate.is_dir()
                    or len(candidate.name) != 64
                    or not is_commit(candidate.name)
                    or binary.is_symlink()
                    or not binary.is_file()
                ):
                    continue
                candidates.append(candidate)
    return candidates


def remove_staging_directory(staging_dir: Path) -> None:
    if staging_dir.is_symlink() or not staging_dir.is_dir():
        raise ProvenanceError(
            f"staging path must be an owner-controlled directory: {staging_dir}"
        )
    staging_dir.chmod(0o700)
    shutil.rmtree(staging_dir)


def reap_orphaned_staging_directories(candidate_root: Path) -> None:
    for staging_dir in candidate_root.iterdir():
        if staging_dir.name.startswith(".staging-"):
            remove_staging_directory(staging_dir)


def prune_staged_candidates(candidate_root: Path, active_candidate: Path) -> None:
    candidates = staged_candidate_directories(candidate_root)
    if len(candidates) <= MAX_STAGED_CANDIDATES:
        return
    candidates.sort(
        key=lambda candidate: (candidate.stat().st_mtime_ns, str(candidate)),
        reverse=True,
    )
    retained = {active_candidate}
    for candidate in candidates:
        if len(retained) >= MAX_STAGED_CANDIDATES:
            break
        retained.add(candidate)
    for candidate in candidates:
        if candidate in retained:
            continue
        state_root = candidate.parent
        commit_root = state_root.parent
        candidate.chmod(0o700)
        shutil.rmtree(candidate)
        for directory in (state_root, commit_root):
            try:
                directory.rmdir()
            except OSError:
                break


def verify_touch_and_prune_candidate(
    repo_root: Path,
    expected: dict[str, str],
    candidate_root: Path,
    final_binary: Path,
    binary_digest: str,
    bundle_digest: str,
    companion_digests: dict[str, str],
) -> dict[str, Any]:
    report = verify_published_candidate(
        repo_root,
        expected,
        final_binary,
        binary_digest,
        bundle_digest,
        companion_digests,
    )
    if report["status"] == "current":
        final_dir = final_binary.parent
        os.utime(final_dir, follow_symlinks=False)
        prune_staged_candidates(candidate_root, final_dir)
    return report


def stage_candidate(
    repo_root: Path,
    binary_path: Path,
    artifact_root: Path,
    companion_paths: list[Path] | None = None,
) -> dict[str, Any]:
    if fcntl is None:
        raise ProvenanceError("content-addressed staging requires POSIX file locking")
    expected = checkout_identity(repo_root)
    if expected["dirty_state"] != "clean":
        return unavailable_report(
            binary_path,
            expected,
            "dirty checkout cannot produce exact dogfood proof; commit or stash tracked changes",
        )
    source = binary_path.expanduser().resolve(strict=True)
    companions = normalize_companion_sources(companion_paths or [])
    artifact_root = artifact_root.expanduser().resolve(strict=False)
    prepare_private_root(artifact_root)
    dogfood_root = artifact_root / "dogfood"
    candidate_root = dogfood_root / "candidates"
    for directory in (artifact_root, dogfood_root, candidate_root):
        ensure_private_directory(directory)
    with (candidate_root / ".stage.lock").open("a", encoding="utf-8") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        reap_orphaned_staging_directories(candidate_root)
        return stage_candidate_locked(
            repo_root, source, companions, candidate_root, expected
        )


def stage_candidate_locked(
    repo_root: Path,
    source: Path,
    companions: dict[str, Path],
    candidate_root: Path,
    expected: dict[str, str],
) -> dict[str, Any]:
    binary_digest = sha256_file(source)
    companion_digests = {name: sha256_file(path) for name, path in companions.items()}
    bundle_digest = candidate_bundle_digest(binary_digest, companion_digests)
    commit_root = candidate_root / expected["source_commit"]
    state_root = commit_root / expected["dirty_state"]
    for directory in (commit_root, state_root):
        ensure_private_directory(directory)
    final_dir = state_root / bundle_digest
    final_binary = final_dir / "codex-lab"
    if final_dir.exists():
        return verify_touch_and_prune_candidate(
            repo_root,
            expected,
            candidate_root,
            final_binary,
            binary_digest,
            bundle_digest,
            companion_digests,
        )

    staging_dir = Path(tempfile.mkdtemp(prefix=".staging-", dir=candidate_root))
    staged_binary = staging_dir / "codex-lab"
    try:
        shutil.copyfile(source, staged_binary)
        staged_binary.chmod(0o555)
        if sha256_file(staged_binary) != binary_digest:
            return unavailable_report(
                staged_binary,
                expected,
                "mutable build changed while it was being staged",
            )
        for name, companion_source in companions.items():
            staged_companion = staging_dir / name
            shutil.copyfile(companion_source, staged_companion)
            staged_companion.chmod(0o555)
            if sha256_file(staged_companion) != companion_digests[name]:
                return unavailable_report(
                    staged_binary,
                    expected,
                    f"mutable companion changed while it was being staged: {name}",
                )
        if sha256_file(source) != binary_digest or any(
            sha256_file(path) != companion_digests[name]
            for name, path in companions.items()
        ):
            return unavailable_report(
                staged_binary,
                expected,
                "mutable build bundle changed while it was being staged",
            )
        staged_report = verification_report(
            expected, staged_binary, reported_provenance(staged_binary)
        )
        if staged_report["status"] != "current":
            return staged_report
        try:
            os.rename(staging_dir, final_dir)
            staging_dir = None
            try:
                final_dir.chmod(0o500)
            except OSError:
                final_dir.chmod(0o700)
                shutil.rmtree(final_dir)
                raise
        except OSError:
            if final_dir.is_symlink() or not final_dir.is_dir():
                raise
        return verify_touch_and_prune_candidate(
            repo_root,
            expected,
            candidate_root,
            final_binary,
            binary_digest,
            bundle_digest,
            companion_digests,
        )
    finally:
        if staging_dir is not None:
            remove_staging_directory(staging_dir)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--artifact-root", type=Path)
    mode.add_argument("--verify-only", action="store_true")
    parser.add_argument(
        "--companion-binary",
        type=Path,
        action="append",
        default=[],
        help="Additional executable to stage beside codex-lab (repeatable)",
    )
    parser.add_argument("--json", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        if args.verify_only:
            report = verify_binary(args.repo_root, args.binary)
        else:
            report = stage_candidate(
                args.repo_root,
                args.binary,
                args.artifact_root,
                args.companion_binary,
            )
    except (OSError, RuntimeError, TypeError, ValueError, ProvenanceError) as error:
        report = unavailable_report(args.binary, None, str(error))

    if args.json:
        print(json.dumps(report, sort_keys=True))
    elif report["status"] == "current":
        print(report["binary_path"])
    else:
        print(f"Codex Lab provenance {report['status']}", file=sys.stderr)
        for failure in report["failures"]:
            print(f"  - {failure}", file=sys.stderr)
    return 0 if report["status"] == "current" else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
