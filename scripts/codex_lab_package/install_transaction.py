"""Durable journal primitives for Codex Lab install and uninstall operations."""

import json
import os
from pathlib import Path
import stat
import uuid

from . import code_route
from .code_route_transaction import MAX_JOURNAL_BYTES
from .code_route_transaction import document_bytes
from .code_route_transaction import durable_write
from .code_route_transaction import fsync_directory
from .code_route_transaction import safe_unlink


JOURNAL_SCHEMA_VERSION = 1
JOURNAL_SUFFIX = ".codex-lab-install-transaction.json"


class InstallTransactionRecoveryError(ValueError):
    """The on-disk installer transaction cannot be safely recovered."""


def journal_path_for_state(state_path: Path) -> Path:
    state_path = code_route.absolute_path(state_path)
    return state_path.with_name(f".{state_path.name}{JOURNAL_SUFFIX}")


def new_transaction_id() -> str:
    return uuid.uuid4().hex


def staged_path_for(target: Path, transaction_id: str) -> Path:
    return target.parent / f".{target.name}.codex-lab-staged-{transaction_id}"


def backup_path_for(target: Path, transaction_id: str) -> Path:
    return target.parent / f".{target.name}.codex-lab-backup-{transaction_id}"


def journal_exists(state_path: Path) -> bool:
    return code_route.path_exists(journal_path_for_state(state_path))


def write_journal(state_path: Path, journal: dict) -> None:
    journal_path = journal_path_for_state(state_path)
    if code_route.path_exists(journal_path):
        raise InstallTransactionRecoveryError(
            f"Codex Lab installer transaction journal already exists: {journal_path}"
        )
    durable_write(journal_path, document_bytes(journal), mode=0o600, replace=False)


def clear_journal(state_path: Path) -> None:
    journal_path = journal_path_for_state(state_path)
    if code_route.path_exists(journal_path):
        safe_unlink(journal_path)


def read_journal(state_path: Path) -> dict:
    state_path = code_route.absolute_path(state_path)
    journal_path = journal_path_for_state(state_path)
    try:
        journal_stat = journal_path.lstat()
    except FileNotFoundError as exc:
        raise InstallTransactionRecoveryError(
            f"Codex Lab installer transaction journal disappeared: {journal_path}"
        ) from exc
    if stat.S_ISLNK(journal_stat.st_mode) or not stat.S_ISREG(journal_stat.st_mode):
        raise InstallTransactionRecoveryError(
            f"Codex Lab installer transaction journal is unsafe: {journal_path}"
        )
    if (
        stat.S_IMODE(journal_stat.st_mode) != 0o600
        or journal_stat.st_uid != os.getuid()
        or journal_stat.st_size > MAX_JOURNAL_BYTES
    ):
        raise InstallTransactionRecoveryError(
            f"Codex Lab installer transaction journal is unsafe: {journal_path}"
        )
    try:
        value = json.loads(journal_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise InstallTransactionRecoveryError(
            f"Could not read Codex Lab installer transaction journal: {journal_path}"
        ) from exc
    validate_journal(value, state_path)
    return value


def validate_journal(value: object, state_path: Path) -> None:
    if (
        not isinstance(value, dict)
        or value.get("schemaVersion") != JOURNAL_SCHEMA_VERSION
    ):
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction journal is malformed"
        )
    transaction_id = value.get("transactionId")
    if (
        not isinstance(transaction_id, str)
        or len(transaction_id) != 32
        or any(character not in "0123456789abcdef" for character in transaction_id)
    ):
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction identifier is malformed"
        )
    if value.get("statePath") != str(state_path):
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction belongs to a different state path"
        )
    operation = value.get("operation")
    if operation not in {"install", "uninstall"}:
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction operation is unsupported"
        )
    for field in (
        "stateBeforeSha256",
        "stateUnreconciledSha256",
        "pendingStateSha256",
        "stateAfterSha256",
    ):
        digest = value.get(field)
        if digest is not None and (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
        ):
            raise InstallTransactionRecoveryError(
                f"Codex Lab installer transaction field {field} is malformed"
            )
    if operation == "install" and (
        value.get("pendingStateSha256") is None
        or value.get("stateAfterSha256") is None
        or value.get("stateUnreconciledSha256") is not None
    ):
        raise InstallTransactionRecoveryError(
            "Codex Lab install transaction state identities are malformed"
        )
    if operation == "uninstall" and (
        value.get("stateBeforeSha256") is None
        or value.get("stateUnreconciledSha256") is None
        or value.get("pendingStateSha256") is not None
        or value.get("stateAfterSha256") is not None
    ):
        raise InstallTransactionRecoveryError(
            "Codex Lab uninstall transaction state identities are malformed"
        )
    targets = value.get("targets")
    if not isinstance(targets, list) or not targets:
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction targets are malformed"
        )
    for target_value in targets:
        if not isinstance(target_value, dict):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction target is malformed"
            )
        target = target_value.get("targetPath")
        if not isinstance(target, str) or not Path(target).is_absolute():
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction target is malformed"
            )
        target_path = Path(target)
        if target_value.get("stagedPath") != str(
            staged_path_for(target_path, transaction_id)
        ):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction staged path is unsafe"
            )
        if target_value.get("backupPath") != str(
            backup_path_for(target_path, transaction_id)
        ):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction backup path is unsafe"
            )
        if not isinstance(target_value.get("wasPresent"), bool):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction target presence is malformed"
            )
        if not isinstance(target_value.get("parentWasPresent"), bool):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction parent presence is malformed"
            )
        retained_backup = target_value.get("retainedBackupPath")
        if retained_backup is not None and (
            not isinstance(retained_backup, str)
            or not Path(retained_backup).is_absolute()
        ):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction retained backup path is malformed"
            )
        if retained_backup is not None and Path(retained_backup) not in {
            state_path.parent / "engine-backup" / "codex",
            state_path.parent / "engine-backup" / "codex-code-mode-host",
        }:
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction retained backup path is unsafe"
            )
        cleanup_boundary = target_value.get("cleanupBoundary")
        if cleanup_boundary is None:
            continue
        if (
            not isinstance(cleanup_boundary, str)
            or not Path(cleanup_boundary).is_absolute()
        ):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction cleanup boundary is malformed"
            )
        target_parent = Path(target_value["targetPath"]).parent
        boundary_path = Path(cleanup_boundary)
        if (
            boundary_path != target_parent
            and boundary_path not in target_parent.parents
        ):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction cleanup boundary is unsafe"
            )
        if target_value["parentWasPresent"] != (boundary_path == target_parent):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction cleanup boundary is inconsistent"
            )
    if operation == "uninstall":
        for field in (
            "engineBackupPath",
            "enginePath",
            "codeModeHostBackupPath",
            "codeModeHostPath",
        ):
            item = value.get(field)
            if item is not None and (
                not isinstance(item, str) or not Path(item).is_absolute()
            ):
                raise InstallTransactionRecoveryError(
                    f"Codex Lab installer transaction field {field} is malformed"
                )


def validate_journal_targets(
    journal: dict,
    *,
    state_path: Path,
    allowed_targets: set[Path],
    required_targets: set[Path],
    expected_engine_path: Path,
    expected_code_mode_host_path: Path,
) -> None:
    target_paths = [Path(target["targetPath"]) for target in journal["targets"]]
    target_set = set(target_paths)
    if len(target_set) != len(target_paths):
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction contains duplicate targets"
        )
    if not target_set <= allowed_targets or not required_targets <= target_set:
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction targets do not match the managed install"
        )
    if state_path not in target_set:
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction does not contain its state path"
        )
    if journal["operation"] != "uninstall":
        return
    expected_engine_backup = state_path.parent / "engine-backup" / "codex"
    expected_host_backup = state_path.parent / "engine-backup" / "codex-code-mode-host"
    if journal.get("engineBackupPath") not in {None, str(expected_engine_backup)}:
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction engine backup path is unsafe"
        )
    if journal.get("codeModeHostBackupPath") not in {
        None,
        str(expected_host_backup),
    }:
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction Code Mode host backup path is unsafe"
        )
    for target in journal["targets"]:
        retained_backup = target.get("retainedBackupPath")
        if retained_backup is None:
            continue
        target_path = Path(target["targetPath"])
        expected_retained = (
            expected_engine_backup
            if target_path == expected_engine_path
            else expected_host_backup
            if target_path == expected_code_mode_host_path
            else None
        )
        if expected_retained is None or retained_backup != str(expected_retained):
            raise InstallTransactionRecoveryError(
                "Codex Lab installer transaction retained backup ownership is unsafe"
            )
    if journal.get("enginePath") != str(expected_engine_path):
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction engine path is unsafe"
        )
    if journal.get("codeModeHostPath") not in {
        None,
        str(expected_code_mode_host_path),
    }:
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction Code Mode host path is unsafe"
        )


def remove_path(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        import shutil

        shutil.rmtree(path)
    elif code_route.path_exists(path):
        path.unlink()
    fsync_directory(path.parent)


def state_sha256(state_path: Path) -> str | None:
    if not code_route.path_exists(state_path):
        return None
    return code_route.sha256_file(state_path)


def require_no_code_route_journal(state_path: Path) -> None:
    from .code_route_transaction import (
        journal_path_for_state as code_route_journal_path,
    )

    journal_path = code_route_journal_path(state_path)
    if code_route.path_exists(journal_path):
        raise InstallTransactionRecoveryError(
            f"Code route transaction must recover before install changes: {journal_path}"
        )
