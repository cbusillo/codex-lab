#!/usr/bin/env python3
"""Own one newly-created POSIX target for the lifetime of a foreground command.

The sidecar lock provides mutual exclusion only. Persisted claims are never a
deletion permit, and an unlocked claim does not prove that its owner crashed.
This module deliberately has no adoption, cleanup, or garbage-collection path.
Cooperative children receive ``CODEX_LAB_OWNED_TARGET`` and the lease
descriptor; no command is automatically routed or adopted into the target.
"""

import argparse
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import stat
import subprocess
import sys
import threading
import time
import uuid
from collections.abc import Mapping, Sequence


SCHEMA_VERSION = 1
REGISTRY_NAME = ".codex-target-ownership"
EXIT_BUSY, EXIT_ACTIVE, EXIT_RELEASED = 75, 20, 21
EXIT_UNKNOWN, EXIT_IDENTITY, EXIT_INTERNAL = 22, 23, 70
CLAIM_LIMIT = 8192
TARGET_LEASE_FD_ENV = "CODEX_LAB_TARGET_LEASE_FD"


class OwnershipError(RuntimeError):
    def __init__(self, reason: str, exit_code: int = EXIT_UNKNOWN) -> None:
        super().__init__(reason)
        self.reason = reason
        self.exit_code = exit_code


def _identity(path: Path, *, kind: str) -> dict[str, object]:
    try:
        info = os.lstat(path)
    except FileNotFoundError as error:
        raise OwnershipError(f"{kind}-missing", EXIT_IDENTITY) from error
    if stat.S_ISLNK(info.st_mode):
        raise OwnershipError(f"{kind}-symlink", EXIT_IDENTITY)
    if not stat.S_ISDIR(info.st_mode):
        raise OwnershipError(f"{kind}-not-directory", EXIT_IDENTITY)
    return {
        "device": info.st_dev,
        "inode": info.st_ino,
        "volumeUuid": None,
        "strength": "weak",
    }


def _root_identity(root: Path) -> tuple[Path, dict[str, object]]:
    if not root.is_absolute():
        raise OwnershipError("root-not-absolute", EXIT_IDENTITY)
    _identity(root, kind="root")
    canonical = root.resolve(strict=False)
    return canonical, _identity(canonical, kind="root")


def _target_name(value: str) -> str:
    path = Path(value)
    if not value or path.is_absolute() or path.name != value or value in {".", ".."}:
        raise OwnershipError("target-name-invalid", EXIT_IDENTITY)
    return value


def _target_key(root: Path, name: str) -> str:
    return hashlib.sha256(f"{root.resolve(strict=False)}\0{name}".encode()).hexdigest()


def _mkdir_private(path: Path) -> None:
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        info = os.lstat(path)
        if (
            stat.S_ISLNK(info.st_mode)
            or not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) & 0o077
        ):
            raise OwnershipError("registry-identity-invalid", EXIT_IDENTITY)


def _lock(path: Path, *, create: bool) -> int:
    flags = os.O_RDWR | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0)
    if create:
        flags |= os.O_CREAT
    try:
        fd = os.open(path, flags, 0o600)
    except FileNotFoundError as error:
        raise OwnershipError("lock-missing", EXIT_UNKNOWN) from error
    except OSError as error:
        raise OwnershipError("lock-identity-invalid", EXIT_UNKNOWN) from error
    try:
        info = os.fstat(fd)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) & 0o077
        ):
            raise OwnershipError("lock-identity-invalid", EXIT_UNKNOWN)
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as error:
        os.close(fd)
        raise OwnershipError("target-lock-held", EXIT_BUSY) from error
    except OwnershipError:
        os.close(fd)
        raise
    except OSError as error:
        os.close(fd)
        raise OwnershipError("lock-identity-invalid", EXIT_UNKNOWN) from error
    return fd


def _close_lock(fd: int) -> None:
    # Closing the descriptor releases this process's flock. An explicit
    # unlock would also release a lock held by an inherited descriptor.
    os.close(fd)


def _fsync_directory(path: Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    fd = os.open(path, flags)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def _write_claim(path: Path, claim: dict[str, object]) -> None:
    registry = path.parent
    temporary = registry / f".{path.name}.{secrets.token_hex(8)}.tmp"
    try:
        fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(claim, stream, sort_keys=True, separators=(",", ":"))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        _fsync_directory(registry)
    except OSError as error:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
        raise OwnershipError("claim-write-failed", EXIT_INTERNAL) from error


def _read_claim(path: Path, key: str) -> dict[str, object] | None:
    flags = os.O_RDONLY | os.O_NONBLOCK | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except FileNotFoundError:
        return None
    except OSError as error:
        raise OwnershipError("claim-identity-invalid", EXIT_UNKNOWN) from error
    try:
        info = os.fstat(fd)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) & 0o077
        ):
            raise OwnershipError("claim-identity-invalid", EXIT_UNKNOWN)
        try:
            payload = os.read(fd, CLAIM_LIMIT + 1)
        except OSError as error:
            raise OwnershipError("claim-read-failed", EXIT_UNKNOWN) from error
    finally:
        os.close(fd)
    if len(payload) > CLAIM_LIMIT:
        raise OwnershipError("claim-oversize", EXIT_UNKNOWN)
    try:
        value = json.loads(payload.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as error:
        raise OwnershipError("claim-malformed", EXIT_UNKNOWN) from error
    if (
        not isinstance(value, dict)
        or value.get("schema") != SCHEMA_VERSION
        or not isinstance(value.get("leaseId"), str)
        or value.get("targetKey") != key
        or value.get("state") not in {"active", "released-unverified"}
        or not isinstance(value.get("events"), list)
        or len(value["events"]) > 2
        or not isinstance(value.get("rootIdentity"), dict)
        or not isinstance(value.get("targetIdentity"), dict)
    ):
        raise OwnershipError("claim-unknown-schema", EXIT_UNKNOWN)
    return value


def _event(state: str, **details: object) -> dict[str, object]:
    return {"state": state, "time": time.time(), **details}


def _result(
    status: str,
    reason: str,
    key: str,
) -> dict[str, object]:
    return {
        "schema": SCHEMA_VERSION,
        "targetKey": key,
        "status": status,
        "reason": reason,
    }


def _inspect(root_value: str, target_value: str) -> tuple[dict[str, object], int]:
    root, root_before = _root_identity(Path(root_value))
    name = _target_name(target_value)
    key = _target_key(root, name)
    registry = root / REGISTRY_NAME
    if not registry.exists():
        return _result("unknown", "registry-missing", key), EXIT_UNKNOWN
    _identity(registry, kind="registry")
    claim_path, lock_path = registry / f"{key}.json", registry / f"{key}.lock"
    claim = _read_claim(claim_path, key)
    if claim is None:
        target = root / name
        if not target.exists():
            return _result("unknown", "claim-missing", key), EXIT_UNKNOWN
        return _result("identity-failure", "target-unowned", key), EXIT_IDENTITY
    root_after = _identity(root, kind="root")
    if root_before != root_after:
        return _result("identity-failure", "root-identity-changed", key), EXIT_IDENTITY
    target = root / name
    try:
        target_identity = _identity(target, kind="target")
    except OwnershipError as error:
        return _result("identity-failure", error.reason, key), EXIT_IDENTITY
    if claim["rootIdentity"] != root_after:
        return _result("identity-failure", "root-claim-mismatch", key), EXIT_IDENTITY
    if claim["targetIdentity"] != target_identity:
        return _result("identity-failure", "target-claim-mismatch", key), EXIT_IDENTITY
    try:
        lock_fd = _lock(lock_path, create=False)
    except OwnershipError as error:
        if error.reason == "target-lock-held" and claim["state"] == "active":
            return _result("active", "lock-held", key), EXIT_ACTIVE
        if (
            error.reason == "target-lock-held"
            and claim["state"] == "released-unverified"
        ):
            return _result(
                "released-unverified", "lease-held-after-release", key
            ), EXIT_RELEASED
        return _result("unknown", error.reason, key), EXIT_UNKNOWN
    else:
        _close_lock(lock_fd)
        if claim["state"] == "active":
            return _result("unknown", "active-claim-lock-free", key), EXIT_UNKNOWN
        return _result(
            "released-unverified", "claim-released-lock-free", key
        ), EXIT_RELEASED


def execute_command(
    command: Sequence[str],
    target: Path,
    lease_fd: int | None = None,
    *,
    cwd: Path | None = None,
    environment: Mapping[str, str] | None = None,
) -> int:
    child: subprocess.Popen[bytes] | None = None
    received_signals: list[int] = []

    def forward(received: int, _frame: object) -> None:
        if not received_signals:
            received_signals.append(received)
        if child is not None and child.poll() is None:
            child.send_signal(received)

    child_env = os.environ.copy() if environment is None else dict(environment)
    child_env["CODEX_LAB_OWNED_TARGET"] = str(target)
    child_env.pop(TARGET_LEASE_FD_ENV, None)
    if lease_fd is not None:
        child_env[TARGET_LEASE_FD_ENV] = str(lease_fd)
    if threading.current_thread() is not threading.main_thread():
        started_child = subprocess.Popen(
            list(command),
            close_fds=True,
            cwd=cwd,
            env=child_env,
            pass_fds=(lease_fd,) if lease_fd is not None else (),
        )
        exit_code = started_child.wait()
        return 128 - exit_code if exit_code < 0 else exit_code
    handlers = {
        number: signal.signal(number, forward)
        for number in (signal.SIGINT, signal.SIGTERM)
    }
    try:
        if received_signals:
            return 128 + received_signals[0]
        started_child = subprocess.Popen(
            list(command),
            close_fds=True,
            cwd=cwd,
            env=child_env,
            pass_fds=(lease_fd,) if lease_fd is not None else (),
        )
        child = started_child
        if received_signals and started_child.poll() is None:
            started_child.send_signal(received_signals[0])
        exit_code = started_child.wait()
        if received_signals:
            return 128 + received_signals[0]
        return 128 - exit_code if exit_code < 0 else exit_code
    finally:
        for number, previous in handlers.items():
            signal.signal(number, previous)


def _run(root_value: str, target_value: str, command: Sequence[str]) -> int:
    if TARGET_LEASE_FD_ENV in os.environ:
        raise OwnershipError("nested-lease-unsupported", EXIT_UNKNOWN)
    if not command:
        raise OwnershipError("command-missing", EXIT_INTERNAL)
    root, root_before = _root_identity(Path(root_value))
    name = _target_name(target_value)
    registry = root / REGISTRY_NAME
    _mkdir_private(registry)
    key = _target_key(root, name)
    claim_path, lock_path = registry / f"{key}.json", registry / f"{key}.lock"
    target_lock = _lock(lock_path, create=True)
    setup_complete = False
    try:
        root_after = _identity(root, kind="root")
        if root_before != root_after:
            raise OwnershipError("root-identity-changed", EXIT_IDENTITY)
        claim = _read_claim(claim_path, key)
        target = root / name
        if claim is not None:
            raise OwnershipError("target-already-claimed", EXIT_UNKNOWN)
        if target.exists() or target.is_symlink():
            raise OwnershipError("target-adoption-required", EXIT_IDENTITY)
        try:
            target.mkdir(mode=0o700)
        except FileExistsError as error:
            raise OwnershipError("target-created-concurrently", EXIT_UNKNOWN) from error
        target_identity = _identity(target, kind="target")
        lease_id = uuid.uuid4().hex
        claim = {
            "schema": SCHEMA_VERSION,
            "leaseId": lease_id,
            "targetKey": key,
            "state": "active",
            "rootIdentity": root_after,
            "targetIdentity": target_identity,
            "events": [_event("active")],
        }
        _write_claim(claim_path, claim)
        setup_complete = True
    finally:
        if not setup_complete:
            _close_lock(target_lock)
    try:
        exit_code = execute_command(command, target, target_lock)
        claim["state"] = "released-unverified"
        events = claim.get("events")
        if not isinstance(events, list):
            raise OwnershipError("claim-events-invalid", EXIT_UNKNOWN)
        events.append(_event("released-unverified", exitCode=exit_code))
        claim["events"] = events
        _write_claim(claim_path, claim)
        return exit_code
    finally:
        _close_lock(target_lock)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="subcommand", required=True)
    run_parser = subparsers.add_parser("run")
    run_parser.add_argument("--root", required=True)
    run_parser.add_argument("--target", required=True)
    run_parser.add_argument("command", nargs=argparse.REMAINDER)
    inspect_parser = subparsers.add_parser("inspect")
    inspect_parser.add_argument("--root", required=True)
    inspect_parser.add_argument("--target", required=True)
    return parser


def main(argv: Sequence[str]) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.subcommand == "inspect":
            result, exit_code = _inspect(args.root, args.target)
            print(json.dumps(result, sort_keys=True))
            return exit_code
        command = args.command[1:] if args.command[:1] == ["--"] else args.command
        return _run(args.root, args.target, command)
    except OwnershipError as error:
        if args.subcommand == "inspect":
            status = (
                "unknown" if error.exit_code == EXIT_UNKNOWN else "identity-failure"
            )
            print(
                json.dumps(
                    _result(status, error.reason, "unknown"),
                    sort_keys=True,
                )
            )
            return error.exit_code
        print(
            json.dumps({"schema": SCHEMA_VERSION, "reason": error.reason}),
            file=sys.stderr,
        )
        return error.exit_code
    except OSError as error:
        if error.errno == errno.ENOTSUP:
            print(
                json.dumps(
                    {"schema": SCHEMA_VERSION, "reason": "posix-lock-unavailable"}
                ),
                file=sys.stderr,
            )
            return EXIT_INTERNAL
        print(
            json.dumps({"schema": SCHEMA_VERSION, "reason": "command-start-failed"}),
            file=sys.stderr,
        )
        return EXIT_INTERNAL


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
