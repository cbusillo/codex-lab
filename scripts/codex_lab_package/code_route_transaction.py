"""Crash-consistent transactions for the explicit ``code`` command route."""

from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
import uuid

from . import code_route


JOURNAL_SCHEMA_VERSION = 1
MAX_JOURNAL_BYTES = 64 * 1024
JOURNAL_SUFFIX = ".code-route-transaction.json"
LOCK_SUFFIX = ".code-route-transaction.lock"


def activate_code_route(
    state_path: Path,
    engine: code_route.CodeRouteEngine,
    *,
    active_path: Path,
    tools: code_route.LauncherTools,
    lock_held: bool = False,
) -> code_route.CodeRouteResult:
    state_path = code_route.absolute_path(state_path)
    active_path = code_route.absolute_path(active_path)
    if not lock_held:
        with transaction_lock(state_path):
            return activate_code_route(
                state_path,
                engine,
                active_path=active_path,
                tools=tools,
                lock_held=True,
            )
    recover_code_route_transaction(
        state_path,
        active_path=active_path,
        lock_held=True,
    )
    state, before_sha256 = code_route.read_state_document_with_sha256(state_path)
    existing = code_route.read_code_route_state(state, state_path)
    if existing is not None:
        code_route.require_active_code_route(existing, expected_path=active_path)
        if existing.engine != engine:
            raise ValueError(
                "Recorded code route engine metadata does not match the verified "
                "managed engine"
            )
        return code_route.CodeRouteResult(
            active_path=active_path,
            changed=False,
            restored_prior=False,
            state_path=state_path,
        )

    code_route.require_safe_parent(active_path.parent)
    code_route.require_safe_parent(state_path.parent)
    code_route.require_exact_engine_path(engine.path)
    transaction_id = uuid.uuid4().hex
    prior_backup_path = active_path.parent / (
        f"{code_route.PRIOR_BACKUP_PREFIX}{transaction_id}"
    )
    launcher_source_path = active_path.parent / (
        f".code.codex-lab-launcher-{transaction_id}"
    )
    prior = code_route.capture_prior_metadata(
        active_path,
        backup_path=prior_backup_path,
    )
    launcher = code_route.build_code_route_launcher_script(engine, tools=tools)
    launcher_sha256 = hashlib.sha256(launcher.encode()).hexdigest()
    route = code_route.CodeRouteState(
        active_path=active_path,
        engine=engine,
        launcher_sha256=launcher_sha256,
        prior=prior,
    )
    after_state = updated_state(state, route)
    after_sha256 = document_sha256(after_state)
    journal = {
        "activeBackupPath": None,
        "activePath": str(active_path),
        "launcherSourcePath": str(launcher_source_path),
        "operation": "activate",
        "routeAfter": code_route.serialize_code_route_state(route),
        "routeBefore": None,
        "schemaVersion": JOURNAL_SCHEMA_VERSION,
        "stateAfterSha256": after_sha256,
        "stateBeforeSha256": before_sha256,
        "statePath": str(state_path),
        "transactionId": transaction_id,
    }
    write_journal(state_path, journal)
    try:
        write_launcher(launcher_source_path, launcher)
        if prior.kind != "absent":
            assert prior.backup_path is not None
            safe_rename(active_path, prior.backup_path)
        safe_rename(launcher_source_path, active_path)
        code_route.write_state_code_route(
            state_path,
            state,
            route,
            expected_sha256=before_sha256,
        )
    except Exception:
        recover_code_route_transaction(
            state_path,
            active_path=active_path,
            lock_held=True,
        )
        raise
    finish_committed_cleanup(state_path, journal)
    require_transaction_clear(state_path)
    return code_route.CodeRouteResult(
        active_path=active_path,
        changed=True,
        restored_prior=False,
        state_path=state_path,
    )


def deactivate_code_route(
    state_path: Path,
    *,
    active_path: Path,
    lock_held: bool = False,
) -> code_route.CodeRouteResult:
    state_path = code_route.absolute_path(state_path)
    active_path = code_route.absolute_path(active_path)
    if not lock_held:
        with transaction_lock(state_path):
            return deactivate_code_route(
                state_path,
                active_path=active_path,
                lock_held=True,
            )
    recover_code_route_transaction(
        state_path,
        active_path=active_path,
        lock_held=True,
    )
    state, before_sha256 = code_route.read_state_document_with_sha256(state_path)
    route = code_route.read_code_route_state(state, state_path)
    if route is None:
        return code_route.CodeRouteResult(
            active_path=active_path,
            changed=False,
            restored_prior=False,
            state_path=state_path,
        )
    code_route.require_active_code_route(route, expected_path=active_path)
    code_route.require_safe_parent(state_path.parent)
    transaction_id = uuid.uuid4().hex
    active_backup_path = active_path.parent / (
        f"{code_route.ACTIVE_BACKUP_PREFIX}{transaction_id}"
    )
    after_state = updated_state(state, None)
    after_sha256 = document_sha256(after_state)
    journal = {
        "activeBackupPath": str(active_backup_path),
        "activePath": str(active_path),
        "launcherSourcePath": None,
        "operation": "deactivate",
        "routeAfter": None,
        "routeBefore": code_route.serialize_code_route_state(route),
        "schemaVersion": JOURNAL_SCHEMA_VERSION,
        "stateAfterSha256": after_sha256,
        "stateBeforeSha256": before_sha256,
        "statePath": str(state_path),
        "transactionId": transaction_id,
    }
    write_journal(state_path, journal)
    try:
        safe_rename(active_path, active_backup_path)
        if route.prior.kind != "absent":
            assert route.prior.backup_path is not None
            safe_rename(route.prior.backup_path, active_path)
        code_route.write_state_code_route(
            state_path,
            state,
            None,
            expected_sha256=before_sha256,
        )
    except Exception:
        recover_code_route_transaction(
            state_path,
            active_path=active_path,
            lock_held=True,
        )
        raise
    finish_committed_cleanup(state_path, journal)
    require_transaction_clear(state_path)
    return code_route.CodeRouteResult(
        active_path=active_path,
        changed=True,
        restored_prior=route.prior.kind != "absent",
        state_path=state_path,
    )


def recover_code_route_transaction(
    state_path: Path,
    *,
    active_path: Path,
    lock_held: bool = False,
) -> None:
    state_path = code_route.absolute_path(state_path)
    active_path = code_route.absolute_path(active_path)
    if not lock_held:
        if not code_route.path_exists(state_path.parent):
            return
        if not state_path.parent.is_dir() and not state_path.parent.is_symlink():
            return
        with transaction_lock(state_path):
            recover_code_route_transaction(
                state_path,
                active_path=active_path,
                lock_held=True,
            )
        return
    journal_path = journal_path_for_state(state_path)
    if not code_route.path_exists(journal_path):
        return
    journal = read_journal(state_path, active_path=active_path)
    current_sha256 = state_sha256(state_path) if state_path.exists() else None
    if current_sha256 == journal["stateBeforeSha256"]:
        rollback_uncommitted(journal)
        clear_journal(state_path)
        return
    if current_sha256 == journal["stateAfterSha256"]:
        finish_committed_cleanup(state_path, journal)
        require_transaction_clear(state_path)
        return
    raise code_route.CodeRouteRecoveryError(
        "Code route transaction state is ambiguous; preserved the journal and "
        f"filesystem for inspection: {journal_path}"
    )


def require_transaction_clear(state_path: Path) -> None:
    journal_path = journal_path_for_state(code_route.absolute_path(state_path))
    if code_route.path_exists(journal_path):
        raise code_route.CodeRouteRecoveryError(
            "Code route transaction cleanup is still pending; retry after the "
            f"journal can be removed: {journal_path}"
        )


def rollback_uncommitted(journal: dict) -> None:
    operation = journal["operation"]
    route_value = (
        journal["routeAfter"] if operation == "activate" else journal["routeBefore"]
    )
    route = read_journal_route(route_value, Path(journal["statePath"]))
    assert route is not None
    if operation == "activate":
        rollback_activation(journal, route)
    elif operation == "deactivate":
        rollback_deactivation(journal, route)
    else:
        raise code_route.CodeRouteRecoveryError(
            f"Unsupported code route transaction operation: {operation}"
        )


def rollback_activation(journal: dict, route: code_route.CodeRouteState) -> None:
    active_path = route.active_path
    launcher_source_path = optional_journal_path(journal, "launcherSourcePath")
    if code_route.path_exists(active_path) and matches_launcher(
        active_path, route.launcher_sha256
    ):
        safe_unlink(active_path)
    if route.prior.kind == "absent":
        if code_route.path_exists(active_path):
            raise recovery_path_error(active_path)
    else:
        assert route.prior.backup_path is not None
        if code_route.path_exists(route.prior.backup_path):
            code_route.require_prior_route(route.prior, active_path)
            if code_route.path_exists(active_path):
                raise recovery_path_error(active_path)
            safe_rename(route.prior.backup_path, active_path)
        elif not matches_prior(active_path, route.prior):
            raise recovery_path_error(active_path)
    if launcher_source_path is not None and code_route.path_exists(
        launcher_source_path
    ):
        if not matches_launcher(launcher_source_path, route.launcher_sha256):
            raise recovery_path_error(launcher_source_path)
        safe_unlink(launcher_source_path)


def rollback_deactivation(journal: dict, route: code_route.CodeRouteState) -> None:
    active_backup_path = required_journal_path(journal, "activeBackupPath")
    if route.prior.kind != "absent" and matches_prior(route.active_path, route.prior):
        assert route.prior.backup_path is not None
        if code_route.path_exists(route.prior.backup_path):
            raise recovery_path_error(route.prior.backup_path)
        safe_rename(route.active_path, route.prior.backup_path)
    if code_route.path_exists(active_backup_path):
        if not matches_launcher(active_backup_path, route.launcher_sha256):
            raise recovery_path_error(active_backup_path)
        if code_route.path_exists(route.active_path):
            raise recovery_path_error(route.active_path)
        safe_rename(active_backup_path, route.active_path)
    elif not matches_launcher(route.active_path, route.launcher_sha256):
        raise recovery_path_error(route.active_path)
    code_route.require_active_code_route(route, expected_path=route.active_path)


def finish_committed_cleanup(state_path: Path, journal: dict) -> None:
    try:
        operation = journal["operation"]
        route_value = (
            journal["routeAfter"] if operation == "activate" else journal["routeBefore"]
        )
        route = read_journal_route(route_value, state_path)
        assert route is not None
        if operation == "activate":
            code_route.require_active_code_route(route, expected_path=route.active_path)
            launcher_source_path = optional_journal_path(journal, "launcherSourcePath")
            if launcher_source_path is not None and code_route.path_exists(
                launcher_source_path
            ):
                if not matches_launcher(launcher_source_path, route.launcher_sha256):
                    raise recovery_path_error(launcher_source_path)
                safe_unlink(launcher_source_path)
        elif operation == "deactivate":
            require_deactivated_layout(route)
            active_backup_path = required_journal_path(journal, "activeBackupPath")
            if code_route.path_exists(active_backup_path):
                if not matches_launcher(active_backup_path, route.launcher_sha256):
                    raise recovery_path_error(active_backup_path)
                safe_unlink(active_backup_path)
        else:
            raise code_route.CodeRouteRecoveryError(
                f"Unsupported code route transaction operation: {operation}"
            )
        clear_journal(state_path)
    except OSError:
        return


def require_deactivated_layout(route: code_route.CodeRouteState) -> None:
    if route.prior.kind == "absent":
        if code_route.path_exists(route.active_path):
            raise recovery_path_error(route.active_path)
        return
    if not matches_prior(route.active_path, route.prior):
        raise recovery_path_error(route.active_path)
    assert route.prior.backup_path is not None
    if code_route.path_exists(route.prior.backup_path):
        raise recovery_path_error(route.prior.backup_path)


def read_journal_route(
    value: object, state_path: Path
) -> code_route.CodeRouteState | None:
    if value is None:
        return None
    if not isinstance(value, dict):
        raise code_route.CodeRouteRecoveryError(
            f"Code route transaction contains invalid route metadata: {state_path}"
        )
    try:
        return code_route.read_code_route_state({"codeRoute": value}, state_path)
    except ValueError as exc:
        raise code_route.CodeRouteRecoveryError(str(exc)) from exc


def read_journal(state_path: Path, *, active_path: Path) -> dict:
    journal_path = journal_path_for_state(state_path)
    try:
        mode = journal_path.lstat().st_mode
    except FileNotFoundError as exc:
        raise code_route.CodeRouteRecoveryError(
            f"Code route transaction journal disappeared: {journal_path}"
        ) from exc
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
        raise code_route.CodeRouteRecoveryError(
            f"Code route transaction journal is unsafe: {journal_path}"
        )
    if stat.S_IMODE(mode) & 0o077:
        raise code_route.CodeRouteRecoveryError(
            f"Code route transaction journal permissions are unsafe: {journal_path}"
        )
    if journal_path.stat().st_size > MAX_JOURNAL_BYTES:
        raise code_route.CodeRouteRecoveryError(
            f"Code route transaction journal is too large: {journal_path}"
        )
    try:
        value = json.loads(journal_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise code_route.CodeRouteRecoveryError(
            f"Could not read code route transaction journal: {journal_path}"
        ) from exc
    validate_journal(value, state_path, active_path=active_path)
    return value


def validate_journal(value: object, state_path: Path, *, active_path: Path) -> None:
    if (
        not isinstance(value, dict)
        or value.get("schemaVersion") != JOURNAL_SCHEMA_VERSION
    ):
        raise code_route.CodeRouteRecoveryError(
            f"Code route transaction journal version is unsupported: {journal_path_for_state(state_path)}"
        )
    required_strings = (
        "activePath",
        "operation",
        "stateAfterSha256",
        "stateBeforeSha256",
        "statePath",
        "transactionId",
    )
    if any(
        not isinstance(value.get(field), str) or not value[field]
        for field in required_strings
    ):
        raise code_route.CodeRouteRecoveryError(
            "Code route transaction journal is malformed"
        )
    if value["operation"] not in {"activate", "deactivate"}:
        raise code_route.CodeRouteRecoveryError(
            f"Unsupported code route transaction operation: {value['operation']}"
        )
    if value["statePath"] != str(state_path):
        raise code_route.CodeRouteRecoveryError(
            "Code route transaction journal belongs to a different state path"
        )
    if len(value["transactionId"]) != 32 or any(
        character not in "0123456789abcdef" for character in value["transactionId"]
    ):
        raise code_route.CodeRouteRecoveryError(
            "Code route transaction identifier is malformed"
        )
    for field in ("stateAfterSha256", "stateBeforeSha256"):
        if len(value[field]) != 64 or any(
            character not in "0123456789abcdef" for character in value[field]
        ):
            raise code_route.CodeRouteRecoveryError(
                f"Code route transaction field {field} is malformed"
            )
    journal_active_path = Path(value["activePath"])
    if not journal_active_path.is_absolute():
        raise code_route.CodeRouteRecoveryError(
            "Code route transaction active path must be absolute"
        )
    if journal_active_path != active_path:
        raise code_route.CodeRouteRecoveryError(
            "Code route transaction journal belongs to a different active path"
        )
    transaction_id = value["transactionId"]
    expected_launcher_source = journal_active_path.parent / (
        f".code.codex-lab-launcher-{transaction_id}"
    )
    expected_active_backup = journal_active_path.parent / (
        f"{code_route.ACTIVE_BACKUP_PREFIX}{transaction_id}"
    )
    if value["operation"] == "activate":
        if value.get("launcherSourcePath") != str(expected_launcher_source):
            raise code_route.CodeRouteRecoveryError(
                "Code route transaction launcher path is unsafe"
            )
        if (
            value.get("activeBackupPath") is not None
            or value.get("routeBefore") is not None
        ):
            raise code_route.CodeRouteRecoveryError(
                "Code route activation journal is malformed"
            )
        route = read_journal_route(value.get("routeAfter"), state_path)
    else:
        if value.get("activeBackupPath") != str(expected_active_backup):
            raise code_route.CodeRouteRecoveryError(
                "Code route transaction backup path is unsafe"
            )
        if (
            value.get("launcherSourcePath") is not None
            or value.get("routeAfter") is not None
        ):
            raise code_route.CodeRouteRecoveryError(
                "Code route deactivation journal is malformed"
            )
        route = read_journal_route(value.get("routeBefore"), state_path)
    if route is None or route.active_path != journal_active_path:
        raise code_route.CodeRouteRecoveryError(
            "Code route transaction route metadata is inconsistent"
        )
    if route.prior.backup_path is not None:
        expected_prior_backup = journal_active_path.parent / (
            f"{code_route.PRIOR_BACKUP_PREFIX}{transaction_id}"
        )
        if route.prior.backup_path != expected_prior_backup:
            raise code_route.CodeRouteRecoveryError(
                "Code route transaction prior backup path is unsafe"
            )


@contextmanager
def transaction_lock(state_path: Path):
    state_path = code_route.absolute_path(state_path)
    lock_root = lock_root_path()
    code_route.require_safe_parent(lock_root.parent)
    lock_root.mkdir(mode=0o700, exist_ok=True)
    lock_root_mode = lock_root.lstat()
    if (
        not stat.S_ISDIR(lock_root_mode.st_mode)
        or stat.S_IMODE(lock_root_mode.st_mode) != 0o700
        or lock_root_mode.st_uid != os.getuid()
    ):
        raise code_route.CodeRouteRecoveryError(
            f"Code route transaction lock directory is unsafe: {lock_root}"
        )
    lock_path = lock_path_for_state(state_path)
    flags = os.O_CREAT | os.O_RDWR
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(lock_path, flags, 0o600)
    try:
        lock_mode = os.fstat(descriptor).st_mode
        if (
            not stat.S_ISREG(lock_mode)
            or stat.S_IMODE(lock_mode) & 0o077
            or os.fstat(descriptor).st_uid != os.getuid()
        ):
            raise code_route.CodeRouteRecoveryError(
                f"Code route transaction lock is unsafe: {lock_path}"
            )
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally:
        os.close(descriptor)


def write_journal(state_path: Path, journal: dict) -> None:
    journal_path = journal_path_for_state(state_path)
    if code_route.path_exists(journal_path):
        raise code_route.CodeRouteRecoveryError(
            f"Code route transaction journal already exists: {journal_path}"
        )
    durable_write(journal_path, document_bytes(journal), mode=0o600, replace=False)


def clear_journal(state_path: Path) -> None:
    journal_path = journal_path_for_state(state_path)
    if not code_route.path_exists(journal_path):
        return
    code_route.require_safe_parent(journal_path.parent)
    journal_path.unlink()
    fsync_directory(journal_path.parent)


def write_launcher(path: Path, launcher: str) -> None:
    durable_write(path, launcher.encode(), mode=0o755, replace=False)


def durable_write(
    path: Path, content: bytes, *, mode: int, replace: bool = True
) -> None:
    code_route.require_safe_parent(path.parent)
    path.parent.mkdir(parents=True, exist_ok=True)
    code_route.require_safe_parent(path.parent)
    if not replace and code_route.path_exists(path):
        raise FileExistsError(f"Transaction path already exists: {path}")
    with tempfile.NamedTemporaryFile(
        "wb",
        dir=path.parent,
        prefix=f".{path.name}.",
        suffix=".tmp",
        delete=False,
    ) as handle:
        temp_path = Path(handle.name)
        handle.write(content)
        handle.flush()
        os.fsync(handle.fileno())
    temp_path.chmod(mode)
    try:
        code_route.require_safe_parent(path.parent)
        if not replace and code_route.path_exists(path):
            raise FileExistsError(f"Transaction path already exists: {path}")
        temp_path.replace(path)
        fsync_directory(path.parent)
    except BaseException:
        temp_path.unlink(missing_ok=True)
        raise


def safe_rename(source: Path, target: Path) -> None:
    code_route.require_safe_parent(source.parent)
    code_route.require_safe_parent(target.parent)
    if code_route.path_exists(target):
        raise FileExistsError(f"Code route transaction target already exists: {target}")
    source.rename(target)
    fsync_directory(source.parent)
    if target.parent != source.parent:
        fsync_directory(target.parent)


def safe_unlink(path: Path) -> None:
    code_route.require_safe_parent(path.parent)
    path.unlink()
    fsync_directory(path.parent)


def matches_launcher(path: Path, expected_sha256: str) -> bool:
    if not code_route.path_exists(path) or path.is_symlink() or not path.is_file():
        return False
    return code_route.sha256_file(path) == expected_sha256


def matches_prior(path: Path, prior: code_route.PriorCodeRoute) -> bool:
    if prior.kind == "absent":
        return not code_route.path_exists(path)
    if not code_route.path_exists(path):
        return False
    mode = path.lstat().st_mode
    if prior.kind == "symlink":
        return stat.S_ISLNK(mode) and os.readlink(path) == prior.symlink_target
    if prior.kind == "regular":
        return (
            stat.S_ISREG(mode)
            and not path.is_symlink()
            and stat.S_IMODE(mode) == prior.mode
            and code_route.sha256_file(path) == prior.sha256
        )
    return False


def recovery_path_error(path: Path) -> code_route.CodeRouteRecoveryError:
    return code_route.CodeRouteRecoveryError(
        f"Code route transaction path does not match recorded ownership: {path}"
    )


def updated_state(
    state: dict,
    route: code_route.CodeRouteState | None,
) -> dict:
    result = dict(state)
    result["codeRoute"] = code_route.serialize_code_route_state(route)
    return result


def state_sha256(state_path: Path) -> str:
    return code_route.sha256_file(state_path)


def document_sha256(value: dict) -> str:
    return hashlib.sha256(document_bytes(value)).hexdigest()


def document_bytes(value: dict) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def journal_path_for_state(state_path: Path) -> Path:
    return state_path.with_name(f".{state_path.name}{JOURNAL_SUFFIX}")


def lock_path_for_state(state_path: Path) -> Path:
    state_digest = hashlib.sha256(str(state_path).encode()).hexdigest()
    return lock_root_path() / f"{state_digest}{LOCK_SUFFIX}"


def lock_root_path() -> Path:
    return Path(tempfile.gettempdir()).resolve() / (
        f"codex-lab-code-route-locks-{os.getuid()}"
    )


def optional_journal_path(journal: dict, field: str) -> Path | None:
    value = journal.get(field)
    return Path(value) if isinstance(value, str) else None


def required_journal_path(journal: dict, field: str) -> Path:
    path = optional_journal_path(journal, field)
    if path is None:
        raise code_route.CodeRouteRecoveryError(
            f"Code route transaction field {field} is missing"
        )
    return path


def fsync_directory(parent: Path) -> None:
    descriptor = os.open(parent, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
