"""Durable journal primitives for Codex Lab install and uninstall operations."""

import json
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


def rewrite_journal(state_path: Path, journal: dict) -> None:
    durable_write(
        journal_path_for_state(state_path), document_bytes(journal), mode=0o600
    )


def clear_journal(state_path: Path) -> None:
    journal_path = journal_path_for_state(state_path)
    if code_route.path_exists(journal_path):
        safe_unlink(journal_path)


def read_journal(state_path: Path) -> dict:
    state_path = code_route.absolute_path(state_path)
    journal_path = journal_path_for_state(state_path)
    try:
        mode = journal_path.lstat().st_mode
    except FileNotFoundError as exc:
        raise InstallTransactionRecoveryError(
            f"Codex Lab installer transaction journal disappeared: {journal_path}"
        ) from exc
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
        raise InstallTransactionRecoveryError(
            f"Codex Lab installer transaction journal is unsafe: {journal_path}"
        )
    if stat.S_IMODE(mode) != 0o600 or journal_path.stat().st_size > MAX_JOURNAL_BYTES:
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
    if value.get("operation") not in {"install", "uninstall"}:
        raise InstallTransactionRecoveryError(
            "Codex Lab installer transaction operation is unsupported"
        )
    for field in ("stateBeforeSha256", "pendingStateSha256", "stateAfterSha256"):
        digest = value.get(field)
        if digest is not None and (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
        ):
            raise InstallTransactionRecoveryError(
                f"Codex Lab installer transaction field {field} is malformed"
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
    if value.get("operation") == "uninstall":
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
