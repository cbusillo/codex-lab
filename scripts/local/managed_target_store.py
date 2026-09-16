"""Private managed Cargo-target store with cooperative lifetime leases.

Discovery and command-family auditing belong to callers. This library owns
only targets it creates below a manifest-pinned root; it never adopts legacy
directories or infers ownership from age, PIDs, locks, or interrupted runs.
"""

import contextlib
from collections.abc import Callable, Iterator, Mapping
import fcntl
import hashlib
import importlib.util
import inspect
import json
import math
import os
from pathlib import Path
import plistlib
import secrets
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from typing import TypedDict, cast
import uuid


SCHEMA_VERSION = 1
DEFAULT_MAX_AGE_DAYS = 14.0
DEFAULT_MIN_IDLE_HOURS = 24.0
DEFAULT_MIN_FREE_BYTES = 200 * 1024**3
DEFAULT_MAX_TARGETS = 1
MAX_TARGETS = 1024
MAX_RECORDS = 4096
MAX_RECORD_BYTES = 16 * 1024
MAX_COMMAND_ARGUMENTS = 256
NONCE_NAME = ".codex-managed-target-nonce"
CARGO_TARGET_ENV = "CARGO_TARGET_DIR"
OWNED_TARGET_ENV = "CODEX_LAB_OWNED_TARGET"
LEASE_FD_ENV = "CODEX_LAB_TARGET_LEASE_FD"
_PRIVATE = 0o700
_FILE = 0o600
_UUID_FIELDS = ("VolumeUUID", "APFSVolumeUUID")
Source = tuple[Path, os.stat_result, Path, os.stat_result, Path, os.stat_result, str]


class VolumeIdentity(TypedDict):
    st_dev: int
    st_ino: int
    uuid: str


class DirectoryIdentity(TypedDict):
    st_ino: int


class RootManifest(TypedDict):
    schema: int
    volume_uuid: str
    managed_root: DirectoryIdentity
    dirs: dict[str, DirectoryIdentity]
    gc_lock: DirectoryIdentity


class ManagedTargetError(RuntimeError):
    """Raised when the store cannot prove an operation safe."""


def verify_volume(path: Path, expected_uuid: str) -> VolumeIdentity:
    """Verify a mounted macOS volume with diskutil and return its identity."""
    if sys.platform != "darwin":
        raise ManagedTargetError("configured volume verification requires macOS")
    try:
        completed = subprocess.run(
            ["diskutil", "info", "-plist", str(path)],
            check=True,
            capture_output=True,
            timeout=10,
        )
        details = plistlib.loads(completed.stdout)
        mounted = details.get("MountPoint")
        actual = next(
            (details.get(key) for key in _UUID_FIELDS if details.get(key)), None
        )
        identity = os.stat(path)
    except (OSError, ValueError, TypeError, subprocess.SubprocessError) as error:
        raise ManagedTargetError(f"volume verification failed: {error}") from error
    try:
        actual = str(uuid.UUID(str(actual)))
    except (ValueError, AttributeError, TypeError) as error:
        raise ManagedTargetError("diskutil returned an invalid volume UUID") from error
    if mounted != str(path) or actual != expected_uuid:
        raise ManagedTargetError("volume mount or UUID does not match configuration")
    return {"st_dev": identity.st_dev, "st_ino": identity.st_ino, "uuid": actual}


def _canonical_dir(path: Path, *, leaf_missing: bool = False) -> Path:
    if not path.is_absolute():
        raise ManagedTargetError("paths must be absolute")
    current = Path(path.anchor)
    parts = path.parts[1:]
    for index, component in enumerate(parts):
        current /= component
        try:
            info = os.lstat(current)
        except FileNotFoundError as error:
            if leaf_missing and index == len(parts) - 1:
                return path
            raise ManagedTargetError(
                f"path component is unavailable: {current}"
            ) from error
        except OSError as error:
            raise ManagedTargetError(
                f"path component cannot be inspected: {current}"
            ) from error
        if stat.S_ISLNK(info.st_mode):
            raise ManagedTargetError(f"symlink path component: {current}")
    try:
        info = os.lstat(path)
    except OSError as error:
        raise ManagedTargetError(f"directory is unavailable: {path}") from error
    if not stat.S_ISDIR(info.st_mode):
        raise ManagedTargetError(f"path is not a directory: {path}")
    return path


def _private_dir(path: Path, *, create: bool = False) -> os.stat_result:
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        if not create:
            raise ManagedTargetError(f"managed directory is missing: {path}")
        try:
            path.mkdir(mode=_PRIVATE)
            info = os.lstat(path)
        except OSError as error:
            raise ManagedTargetError(
                f"managed directory cannot be created: {path}"
            ) from error
    except OSError as error:
        raise ManagedTargetError(
            f"managed directory cannot be inspected: {path}"
        ) from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.geteuid()
        or stat.S_IMODE(info.st_mode) & 0o077
    ):
        raise ManagedTargetError(f"managed directory identity is invalid: {path}")
    return info


def _private_file(path: Path, *, create: bool = False) -> int:
    flags = os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
    if create:
        flags |= os.O_CREAT
    try:
        fd = os.open(path, flags, _FILE)
    except OSError as error:
        raise ManagedTargetError(f"managed file cannot be opened: {path}") from error
    try:
        info = os.fstat(fd)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) & 0o077
        ):
            raise ManagedTargetError(f"managed file identity is invalid: {path}")
        return fd
    except BaseException:
        os.close(fd)
        raise


def _read_json(path: Path) -> dict[str, object]:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except OSError as error:
        raise ManagedTargetError(f"managed JSON cannot be opened: {path}") from error
    try:
        info = os.fstat(fd)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.geteuid()
            or stat.S_IMODE(info.st_mode) & 0o077
            or info.st_size > MAX_RECORD_BYTES
        ):
            raise ManagedTargetError(f"managed JSON identity is invalid: {path}")
        payload = os.read(fd, MAX_RECORD_BYTES + 1)
    except OSError as error:
        raise ManagedTargetError(f"managed JSON cannot be opened: {path}") from error
    finally:
        os.close(fd)
    if len(payload) > MAX_RECORD_BYTES:
        raise ManagedTargetError("managed JSON exceeds per-record bound")
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ManagedTargetError(f"managed JSON is malformed: {path}") from error
    if not isinstance(value, dict):
        raise ManagedTargetError(f"managed JSON must be an object: {path}")
    return value


def _write_json(path: Path, value: Mapping[str, object]) -> None:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    if len(encoded) > MAX_RECORD_BYTES:
        raise ManagedTargetError("managed JSON exceeds per-record bound")
    fd, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(name)
    try:
        os.fchmod(fd, _FILE)
        with os.fdopen(fd, "wb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        if path.is_symlink():
            raise ManagedTargetError(f"refusing symlink replacement: {path}")
        os.replace(temporary, path)
    except BaseException:
        with contextlib.suppress(FileNotFoundError):
            temporary.unlink()
        raise


def _identity(path: Path) -> tuple[int, int]:
    try:
        info = os.lstat(path)
    except OSError as error:
        raise ManagedTargetError("managed target is unavailable") from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.geteuid()
        or stat.S_IMODE(info.st_mode) & 0o077
    ):
        raise ManagedTargetError("managed target identity is invalid")
    return info.st_dev, info.st_ino


def _source_lstat(path: Path) -> os.stat_result | None:
    """Inspect original components; ENOENT is the only retired signal."""
    if len(path.parts) < 2:
        raise ManagedTargetError("source path is not a directory path")
    current = Path(path.anchor)
    info = os.lstat(current)
    for component in path.parts[1:]:
        current /= component
        try:
            info = os.lstat(current)
        except FileNotFoundError:
            return None
        except OSError as error:
            raise ManagedTargetError("source identity cannot be inspected") from error
        if stat.S_ISLNK(info.st_mode):
            raise ManagedTargetError("source identity is replaced by a symlink")
    if not stat.S_ISDIR(info.st_mode):
        raise ManagedTargetError("source identity is not a directory")
    return info


def _read_nonce(path: Path) -> str:
    fd = _private_file(path)
    try:
        return os.read(fd, 128).decode("ascii")
    except (OSError, UnicodeDecodeError) as error:
        raise ManagedTargetError("managed source nonce is unreadable") from error
    finally:
        os.close(fd)


def _lock(fd: int, *, blocking: bool = False) -> None:
    flags = fcntl.LOCK_EX if blocking else fcntl.LOCK_EX | fcntl.LOCK_NB
    try:
        fcntl.flock(fd, flags)
    except OSError as error:
        raise ManagedTargetError("managed lock is held") from error


class ManagedTargetStore:
    """Create, lease, and boundedly collect targets below one private root."""

    def __init__(
        self,
        config: dict,
        *,
        volume_validator: Callable[[Path, str], Mapping[str, object]] | None = None,
        clock: Callable[[], float] | None = None,
    ) -> None:
        if not isinstance(config, dict) or config.get("schema") != SCHEMA_VERSION:
            raise ManagedTargetError("config schema must be 1")
        self.volume_root = self._config_path(config, "volume_root")
        self.managed_root = self._config_path(config, "managed_root", leaf_missing=True)
        if self.managed_root == self.volume_root:
            raise ManagedTargetError("managed_root must be a strict descendant")
        try:
            self.managed_root.relative_to(self.volume_root)
            self.volume_uuid = str(uuid.UUID(str(config["volume_uuid"])))
        except (KeyError, ValueError, TypeError) as error:
            raise ManagedTargetError(
                "managed_root or volume_uuid is invalid"
            ) from error
        self.max_age_days = self._positive(
            config.get("max_age_days", DEFAULT_MAX_AGE_DAYS), "max_age_days"
        )
        self.min_idle_hours = self._positive(
            config.get("min_idle_hours", DEFAULT_MIN_IDLE_HOURS), "min_idle_hours"
        )
        self.min_free_bytes = self._nonnegative(
            config.get("min_free_bytes", DEFAULT_MIN_FREE_BYTES), "min_free_bytes"
        )
        self.max_targets = self._bounded(
            config.get("max_targets", DEFAULT_MAX_TARGETS),
            "max_targets",
            1,
            MAX_TARGETS,
        )
        self.kache_gc = config.get("kache_gc", False)
        if (
            not isinstance(self.kache_gc, bool)
            or not math.isfinite(self.max_age_days * 86400)
            or not math.isfinite(self.min_idle_hours * 3600)
        ):
            raise ManagedTargetError("invalid retention or kache_gc setting")
        self._volume_validator = volume_validator or verify_volume
        self._clock = clock or time.time

    @staticmethod
    def _config_path(
        config: Mapping[str, object], name: str, *, leaf_missing: bool = False
    ) -> Path:
        value = config.get(name)
        path = Path(value) if isinstance(value, str) else Path()
        if (
            not isinstance(value, str)
            or not value
            or not path.is_absolute()
            or ".." in path.parts
        ):
            raise ManagedTargetError(f"{name} must be an absolute clean path")
        return _canonical_dir(path, leaf_missing=leaf_missing)

    @staticmethod
    def _positive(value: object, name: str) -> float:
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ManagedTargetError(f"{name} must be finite and positive")
        result = float(value)
        if not math.isfinite(result) or result <= 0:
            raise ManagedTargetError(f"{name} must be finite and positive")
        return result

    @staticmethod
    def _nonnegative(value: object, name: str) -> int:
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise ManagedTargetError(f"{name} must be a nonnegative integer")
        return value

    @staticmethod
    def _bounded(value: object, name: str, lower: int, upper: int) -> int:
        if (
            isinstance(value, bool)
            or not isinstance(value, int)
            or not lower <= value <= upper
        ):
            raise ManagedTargetError(f"{name} must be between {lower} and {upper}")
        return value

    def _volume(self) -> VolumeIdentity:
        _canonical_dir(self.volume_root)
        try:
            value = self._volume_validator(self.volume_root, self.volume_uuid)
        except ManagedTargetError:
            raise
        except Exception as error:
            raise ManagedTargetError("volume validator failed") from error
        if not isinstance(value, Mapping):
            raise ManagedTargetError("volume validator returned wrong UUID")
        raw_uuid, device, inode = (
            value.get("uuid"),
            value.get("st_dev"),
            value.get("st_ino"),
        )
        if (
            not isinstance(raw_uuid, str)
            or raw_uuid != self.volume_uuid
            or type(device) is not int
            or type(inode) is not int
        ):
            raise ManagedTargetError("volume validator returned invalid identity")
        if device < 0 or inode <= 0:
            raise ManagedTargetError("volume validator returned invalid identity")
        return {"st_dev": device, "st_ino": inode, "uuid": self.volume_uuid}

    @property
    def _manifest_path(self) -> Path:
        return self.managed_root / "manifest.json"

    @property
    def _registry_path(self) -> Path:
        return self.managed_root / "registry"

    @property
    def _targets_path(self) -> Path:
        return self.managed_root / "targets"

    @property
    def _quarantine_path(self) -> Path:
        return self.managed_root / "quarantine"

    @property
    def _gc_lock_path(self) -> Path:
        return self.managed_root / "gc.lock"

    def _manifest(self, volume: VolumeIdentity, root: os.stat_result) -> RootManifest:
        dirs: dict[str, DirectoryIdentity] = {
            name: {"st_ino": info.st_ino}
            for name, info in (
                ("registry", os.lstat(self._registry_path)),
                ("targets", os.lstat(self._targets_path)),
                ("quarantine", os.lstat(self._quarantine_path)),
            )
        }
        lock_info = os.lstat(self._gc_lock_path)
        return {
            "schema": SCHEMA_VERSION,
            "volume_uuid": volume["uuid"],
            "managed_root": {"st_ino": root.st_ino},
            "dirs": dirs,
            "gc_lock": {"st_ino": lock_info.st_ino},
        }

    def initialize(self) -> None:
        volume = self._volume()
        _canonical_dir(self.managed_root.parent)
        existed = self.managed_root.exists() or self.managed_root.is_symlink()
        root = _private_dir(self.managed_root, create=True)
        if existed and not self._manifest_path.exists():
            raise ManagedTargetError("preexisting managed root has no own manifest")
        if not self._manifest_path.exists() and not self._manifest_path.is_symlink():
            _private_dir(self._registry_path, create=True)
            _private_dir(self._targets_path, create=True)
            _private_dir(self._quarantine_path, create=True)
            fd = _private_file(self._gc_lock_path, create=True)
            os.close(fd)
            _write_json(self._manifest_path, self._manifest(volume, root))
            return
        self._ensure()

    def _ensure(self) -> tuple[RootManifest, int]:
        volume = self._volume()
        root = _private_dir(self.managed_root)
        if root.st_dev != volume["st_dev"]:
            raise ManagedTargetError("managed root is on the wrong volume")
        current_dirs: dict[str, DirectoryIdentity] = {}
        for name, path in (
            ("registry", self._registry_path),
            ("targets", self._targets_path),
            ("quarantine", self._quarantine_path),
        ):
            info = _private_dir(path)
            if info.st_dev != volume["st_dev"]:
                raise ManagedTargetError(f"managed {name} is on the wrong volume")
            current_dirs[name] = {"st_ino": info.st_ino}
        fd = _private_file(self._gc_lock_path)
        try:
            lock_info = os.fstat(fd)
        finally:
            os.close(fd)
        if lock_info.st_dev != volume["st_dev"]:
            raise ManagedTargetError("managed GC lock is on the wrong volume")
        expected = self._manifest(volume, root)
        expected["dirs"] = current_dirs
        expected["gc_lock"] = DirectoryIdentity(st_ino=lock_info.st_ino)
        if _read_json(self._manifest_path) != expected:
            raise ManagedTargetError(
                "managed root manifest or directory identity changed"
            )
        return expected, volume["st_dev"]

    @staticmethod
    def _safe_key(key: str) -> bool:
        return len(key) == 64 and all(char in "0123456789abcdef" for char in key)

    def _record_path(self, key: str) -> Path:
        if not self._safe_key(key):
            raise ManagedTargetError("managed record key is malformed")
        return self._registry_path / f"{key}.json"

    def _load_record(self, key: str) -> dict[str, object] | None:
        path = self._record_path(key)
        if not path.exists() and not path.is_symlink():
            return None
        return _read_json(path)

    def _save_record(self, key: str, record: Mapping[str, object]) -> None:
        _write_json(self._record_path(key), record)

    @staticmethod
    def _worktree(path: Path) -> tuple[Path, os.stat_result]:
        canonical = _canonical_dir(path)
        try:
            return canonical, os.stat(canonical)
        except OSError as error:
            raise ManagedTargetError("worktree identity is unavailable") from error

    def _source(self, worktree: Path, *, create_nonce: bool) -> Source:
        worktree, worktree_info = self._worktree(worktree)
        try:
            result = subprocess.run(
                ["git", "rev-parse", "--absolute-git-dir", "--git-common-dir"],
                cwd=worktree,
                check=True,
                capture_output=True,
                text=True,
                timeout=10,
            )
            gitdir, common = (Path(line) for line in result.stdout.splitlines())
        except (OSError, subprocess.SubprocessError, ValueError) as error:
            raise ManagedTargetError("cannot resolve worktree Git identity") from error
        gitdir = Path(
            os.path.realpath(gitdir if gitdir.is_absolute() else worktree / gitdir)
        )
        common = Path(
            os.path.realpath(common if common.is_absolute() else worktree / common)
        )
        _canonical_dir(gitdir)
        _canonical_dir(common)
        gitdir_info, common_info = os.stat(gitdir), os.stat(common)
        nonce_path = gitdir / NONCE_NAME
        try:
            os.lstat(nonce_path)
        except FileNotFoundError:
            if not create_nonce:
                raise ManagedTargetError("worktree managed nonce is missing")
            nonce = secrets.token_hex(16)
            fd, name = tempfile.mkstemp(prefix=f".{NONCE_NAME}.", dir=gitdir)
            try:
                os.fchmod(fd, _FILE)
                with os.fdopen(fd, "w", encoding="ascii") as stream:
                    stream.write(nonce)
                    stream.flush()
                    os.fsync(stream.fileno())
                try:
                    os.link(name, nonce_path)
                except FileExistsError:
                    nonce = _read_nonce(nonce_path)
                os.unlink(name)
            except BaseException:
                with contextlib.suppress(FileNotFoundError):
                    os.unlink(name)
                raise
        else:
            nonce = _read_nonce(nonce_path)
        if len(nonce) != 32 or any(char not in "0123456789abcdef" for char in nonce):
            raise ManagedTargetError("worktree managed nonce is malformed")
        return worktree, worktree_info, gitdir, gitdir_info, common, common_info, nonce

    @staticmethod
    def _key(source: Source) -> str:
        worktree, _, gitdir, _, common, _, nonce = source
        return hashlib.sha256(
            f"{worktree}\0{gitdir}\0{common}\0{nonce}".encode()
        ).hexdigest()

    def target_for(self, worktree: Path) -> Path:
        """Return a known source's target path without creating or adopting it."""
        self._ensure()
        return self._targets_path / self._key(
            self._source(Path(worktree), create_nonce=False)
        )

    @contextlib.contextmanager
    def _lease(self, key: str, *, create: bool) -> Iterator[int]:
        path = self._targets_path / f"{key}.lock"
        fd = _private_file(path, create=create)
        try:
            _lock(fd)
            yield fd
        finally:
            os.close(fd)

    def _entry(
        self, source: Source, key: str, target: tuple[int, int], lock: tuple[int, int]
    ) -> dict[str, object]:
        worktree, wi, gitdir, gi, common, ci, nonce = source
        return {
            "schema": SCHEMA_VERSION,
            "key": key,
            "worktree": str(worktree),
            "worktree_st_ino": wi.st_ino,
            "gitdir": str(gitdir),
            "gitdir_st_ino": gi.st_ino,
            "common_gitdir": str(common),
            "common_gitdir_st_ino": ci.st_ino,
            "nonce": nonce,
            "target_st_ino": target[1],
            "lock_st_ino": lock[1],
            "owner_retired": False,
            "state": "active",
            "last_used": self._now(),
        }

    def _now(self) -> float:
        value = float(self._clock())
        if not math.isfinite(value) or value < 0:
            raise ManagedTargetError("clock returned invalid timestamp")
        return value

    @staticmethod
    def _source_matches(record: Mapping[str, object], source: Source, key: str) -> None:
        worktree, wi, gitdir, gi, common, ci, nonce = source
        expected = {
            "schema": SCHEMA_VERSION,
            "key": key,
            "worktree": str(worktree),
            "worktree_st_ino": wi.st_ino,
            "gitdir": str(gitdir),
            "gitdir_st_ino": gi.st_ino,
            "common_gitdir": str(common),
            "common_gitdir_st_ino": ci.st_ino,
            "nonce": nonce,
        }
        if any(record.get(name) != value for name, value in expected.items()):
            raise ManagedTargetError("managed source identity mismatch")

    def run(self, worktree: Path, command: list[str], cwd: Path) -> int:
        """Run one cooperative command with a target lease inherited by children."""
        _, volume_dev = self._ensure()
        if (
            not isinstance(command, list)
            or not command
            or len(command) > MAX_COMMAND_ARGUMENTS
            or any(not isinstance(arg, str) or not arg for arg in command)
        ):
            raise ManagedTargetError("command must be a bounded nonempty list")
        if any(
            name in os.environ
            for name in (CARGO_TARGET_ENV, OWNED_TARGET_ENV, LEASE_FD_ENV)
        ):
            raise ManagedTargetError(
                "inherited target override or nested lease refused"
            )
        if self._free_bytes() < self.min_free_bytes:
            raise ManagedTargetError("managed volume is below free-space floor")
        source = self._source(Path(worktree), create_nonce=True)
        cwd, _ = self._worktree(Path(cwd))
        try:
            cwd.relative_to(source[0])
        except ValueError as error:
            raise ManagedTargetError("cwd must be inside worktree") from error
        key, target = self._key(source), self._targets_path / self._key(source)
        existing = self._load_record(key)
        with self._lease(key, create=existing is None) as lease_fd:
            _, volume_dev = self._ensure()
            record = self._load_record(key)
            lock_info = os.fstat(lease_fd)
            if record is not None:
                self._source_matches(record, source, key)
                # A new explicit managed invocation can resume its own target
                # after interruption, but only after acquiring and validating
                # the exclusive lease. Automatic GC still requires completion.
                if record.get("state") not in {"managed-idle", "active"}:
                    raise ManagedTargetError(
                        "target is protected by interrupted collection or unknown state"
                    )
                target_identity = _identity(target)
                if target_identity[0] != volume_dev or target_identity[1] != record.get(
                    "target_st_ino"
                ):
                    raise ManagedTargetError("managed target identity mismatch")
                if (
                    lock_info.st_dev != volume_dev
                    or record.get("lock_st_ino") != lock_info.st_ino
                ):
                    raise ManagedTargetError("managed target lease identity mismatch")
            else:
                if target.exists() or target.is_symlink():
                    raise ManagedTargetError("unregistered target will not be adopted")
                target.mkdir(mode=_PRIVATE)
                target_identity = _identity(target)
                if target_identity[0] != volume_dev:
                    raise ManagedTargetError("new target is on the wrong volume")
                record = self._entry(
                    source, key, target_identity, (lock_info.st_dev, lock_info.st_ino)
                )
            active = dict(record)
            active.update({"state": "active", "last_used": self._now()})
            self._save_record(key, active)
            environment = os.environ.copy()
            environment.update(
                {
                    CARGO_TARGET_ENV: str(target),
                    OWNED_TARGET_ENV: str(target),
                    LEASE_FD_ENV: str(lease_fd),
                    # The prestarted daemon owns periodic GC. A wrapper-spawned
                    # GC would inherit this lease and delay the next build.
                    "KACHE_AUTO_GC": "0",
                }
            )
            completed = self._execute_shared(
                command, target, lease_fd, cwd=cwd, environment=environment
            )
            if 0 <= completed <= 125:
                current = self._load_record(key)
                if current is not None and current.get("state") == "active":
                    idle = dict(current)
                    idle.update({"state": "managed-idle", "last_used": self._now()})
                    self._save_record(key, idle)
            return completed

    @staticmethod
    def _execute_shared(
        command: list[str],
        target: Path,
        lease_fd: int,
        *,
        cwd: Path,
        environment: dict[str, str],
    ) -> int:
        try:
            from target_ownership import execute_command
        except ImportError:
            path = Path(__file__).with_name("target_ownership.py")
            spec = importlib.util.spec_from_file_location("target_ownership", path)
            if spec is None or spec.loader is None:
                raise ManagedTargetError("shared target executor is unavailable")
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            execute_command = module.execute_command
        return execute_command(
            command, target, lease_fd, cwd=cwd, environment=environment
        )

    def _free_bytes(self) -> int:
        try:
            usage = os.statvfs(self.volume_root)
            available = usage.f_bavail * usage.f_frsize
        except (AttributeError, OSError) as error:
            raise ManagedTargetError("volume free space is unavailable") from error
        if available < 0:
            raise ManagedTargetError("volume free space is invalid")
        return available

    def collect(self, apply: bool = False) -> dict[str, object]:
        """Return a bounded registry report and optionally delete one target."""
        if not isinstance(apply, bool):
            raise ManagedTargetError("apply must be boolean")
        layout, volume_dev = self._ensure()
        now, cutoff, grace = (
            self._now(),
            self.max_age_days * 86400,
            self.min_idle_hours * 3600,
        )
        gc_fd: int | None = None
        try:
            if apply:
                opened_gc_fd = _private_file(self._gc_lock_path)
                gc_fd = opened_gc_fd
                gc_info = os.fstat(opened_gc_fd)
                if (
                    gc_info.st_dev != volume_dev
                    or gc_info.st_ino != layout["gc_lock"]["st_ino"]
                ):
                    raise ManagedTargetError("managed GC lock identity changed")
                try:
                    _lock(opened_gc_fd)
                except ManagedTargetError:
                    return {
                        "schema": SCHEMA_VERSION,
                        "under_pressure": None,
                        "actual_free_bytes": None,
                        "selected_ids": [],
                        "inventory": [],
                        "actions": [],
                        "reason": "gc-busy",
                    }
            before, inventory, candidates = self._free_bytes(), [], []
            volume_dev = self._volume()["st_dev"]
            names = []
            for index, path in enumerate(self._registry_path.iterdir()):
                if index >= MAX_RECORDS:
                    raise ManagedTargetError("managed registry exceeds record bound")
                if path.name.endswith(".json"):
                    names.append(path.name)
            names.sort()
            pressure = before < self.min_free_bytes
            for name in names:
                key = name[:-5]
                if not self._safe_key(key):
                    inventory.append({"id": key, "reason": "corrupt-entry"})
                    continue
                try:
                    record = self._load_record(key)
                except ManagedTargetError:
                    inventory.append({"id": key, "reason": "corrupt-entry"})
                    continue
                if record is None:
                    continue
                item, candidate = self._inspect_record(key, record, now, volume_dev)
                inventory.append(item)
                if candidate is not None and (
                    now - candidate[1] >= cutoff
                    or pressure
                    and now - candidate[1] >= grace
                ):
                    candidates.append(candidate)
            candidates.sort(key=lambda value: (not value[2], value[1], value[0]))
            selected = candidates[: self.max_targets]
            selected_ids = [value[0] for value in selected]
            for item in inventory:
                if item.get("id") in selected_ids:
                    item["reason"] = "selected"
                elif item.get("reason") == "eligible-idle":
                    age = item["age_seconds"]
                    if age >= cutoff or pressure and age >= grace:
                        item["reason"] = "not-selected-cap"
                    else:
                        item["reason"] = (
                            "recent-pressure-grace" if pressure else "young"
                        )
            actions = (
                [self._apply_one(key, timestamp) for key, timestamp, _ in selected]
                if apply
                else []
            )
            after = self._free_bytes()
            return {
                "schema": SCHEMA_VERSION,
                "under_pressure": pressure,
                "actual_free_bytes": after,
                "selected_ids": selected_ids,
                "inventory": inventory,
                "actions": actions,
            }
        finally:
            if gc_fd is not None:
                os.close(gc_fd)

    def _inspect_record(
        self, key: str, record: Mapping[str, object], now: float, volume_dev: int
    ) -> tuple[dict[str, object], tuple[str, float, bool] | None]:
        item: dict[str, object] = {"id": key, "state": record.get("state", "unknown")}
        if record.get("state") != "managed-idle":
            item["reason"] = "protected-nonidle"
            return item, None
        try:
            source, source_missing = self._source_from_record(record, key)
            if not source_missing:
                self._source_matches(record, source, key)
            target_info = _identity(self._targets_path / key)
            if target_info[0] != volume_dev or target_info[1] != record.get(
                "target_st_ino"
            ):
                raise ManagedTargetError("target identity mismatch")
            lock_fd = _private_file(self._targets_path / f"{key}.lock")
            try:
                lock_info = os.fstat(lock_fd)
                if lock_info.st_dev != volume_dev or lock_info.st_ino != record.get(
                    "lock_st_ino"
                ):
                    raise ManagedTargetError("target lease identity mismatch")
                try:
                    _lock(lock_fd)
                except ManagedTargetError:
                    item["reason"] = "lease-held"
                    return item, None
            finally:
                os.close(lock_fd)
        except (ManagedTargetError, OSError, KeyError):
            item["reason"] = "identity-invalid"
            return item, None
        timestamp, retired = record.get("last_used"), record.get("owner_retired")
        if (
            isinstance(timestamp, bool)
            or not isinstance(timestamp, (int, float))
            or not isinstance(retired, bool)
        ):
            item["reason"] = "corrupt-entry"
            return item, None
        try:
            timestamp = float(timestamp)
        except (OverflowError, ValueError):
            item["reason"] = "invalid-timestamp"
            return item, None
        if not math.isfinite(timestamp) or timestamp < 0:
            item["reason"] = "invalid-timestamp"
            return item, None
        if timestamp > now:
            item["reason"] = "future-timestamp"
            return item, None
        item.update({"reason": "eligible-idle", "age_seconds": now - timestamp})
        return item, (key, timestamp, retired or source_missing)

    def _source_from_record(
        self, record: Mapping[str, object], key: str
    ) -> tuple[Source, bool]:
        if record.get("schema") != SCHEMA_VERSION or record.get("key") != key:
            raise ManagedTargetError("source record schema or key is invalid")
        raw_paths = tuple(
            record.get(name) for name in ("worktree", "gitdir", "common_gitdir")
        )
        if any(not isinstance(path, str) for path in raw_paths):
            raise ManagedTargetError("source record paths are malformed")
        paths = tuple(Path(cast(str, path)) for path in raw_paths)
        nonce = record.get("nonce")
        if (
            any(not path.is_absolute() or ".." in path.parts for path in paths)
            or not isinstance(nonce, str)
            or len(nonce) != 32
            or any(char not in "0123456789abcdef" for char in nonce)
        ):
            raise ManagedTargetError("source record is malformed")
        values: list[os.stat_result | None] = []
        for path in paths:
            info = _source_lstat(path)
            values.append(info)
        for path, info, field in zip(
            paths, values, ("worktree_st_ino", "gitdir_st_ino", "common_gitdir_st_ino")
        ):
            if info is not None and info.st_ino != record.get(field):
                raise ManagedTargetError("source identity changed")
        if values[1] is not None:
            if _read_nonce(paths[1] / NONCE_NAME) != nonce:
                raise ManagedTargetError("source nonce changed")
        empty = os.stat_result((0,) * 10)
        source = (
            paths[0],
            values[0] or empty,
            paths[1],
            values[1] or empty,
            paths[2],
            values[2] or empty,
            nonce,
        )
        if self._key(source) != key:
            raise ManagedTargetError("source key mismatch")
        return source, any(info is None for info in values)

    def _apply_one(self, key: str, selected_timestamp: float) -> dict[str, object]:
        try:
            with self._lease(key, create=False) as lease_fd:
                layout, volume_dev = self._ensure()
                record = self._load_record(key)
                if (
                    record is None
                    or record.get("state") != "managed-idle"
                    or record.get("last_used") != selected_timestamp
                ):
                    return {
                        "id": key,
                        "status": "protected",
                        "reason": "revalidation-failed",
                    }
                source, source_missing = self._source_from_record(record, key)
                if not source_missing:
                    self._source_matches(record, source, key)
                if (
                    not getattr(shutil.rmtree, "avoids_symlink_attacks", False)
                    or "dir_fd" not in inspect.signature(shutil.rmtree).parameters
                ):
                    return {
                        "id": key,
                        "status": "protected",
                        "reason": "safe-recursive-delete-unavailable",
                    }
                target = self._targets_path / key
                identity = _identity(target)
                info = os.fstat(lease_fd)
                if (
                    identity[0] != volume_dev
                    or record.get("target_st_ino") != identity[1]
                    or record.get("lock_st_ino") != info.st_ino
                    or info.st_dev != volume_dev
                ):
                    return {
                        "id": key,
                        "status": "protected",
                        "reason": "identity-mismatch",
                    }
                quarantine = self._quarantine_path / f"{key}.{secrets.token_hex(12)}"
                nofollow = getattr(os, "O_NOFOLLOW", 0)
                target_fd = os.open(
                    self._targets_path,
                    os.O_RDONLY | nofollow | getattr(os, "O_DIRECTORY", 0),
                )
                try:
                    quarantine_fd = os.open(
                        self._quarantine_path,
                        os.O_RDONLY | nofollow | getattr(os, "O_DIRECTORY", 0),
                    )
                    expected_dirs = layout["dirs"]
                    if any(
                        os.fstat(directory_fd).st_dev != volume_dev
                        or os.fstat(directory_fd).st_ino
                        != expected_dirs[name]["st_ino"]
                        for name, directory_fd in (
                            ("targets", target_fd),
                            ("quarantine", quarantine_fd),
                        )
                    ):
                        return {
                            "id": key,
                            "status": "protected",
                            "reason": "managed-directory-volume-mismatch",
                        }
                    target_child_fd = os.open(
                        key,
                        os.O_RDONLY | nofollow | getattr(os, "O_DIRECTORY", 0),
                        dir_fd=target_fd,
                    )
                    try:
                        target_child_info = os.fstat(target_child_fd)
                    finally:
                        os.close(target_child_fd)
                    if (target_child_info.st_dev, target_child_info.st_ino) != identity:
                        return {
                            "id": key,
                            "status": "protected",
                            "reason": "target-revalidation-failed",
                        }
                    deleting = dict(record)
                    deleting.update(
                        {"state": "deleting", "quarantine": quarantine.name}
                    )
                    self._save_record(key, deleting)
                    os.replace(
                        key,
                        quarantine.name,
                        src_dir_fd=target_fd,
                        dst_dir_fd=quarantine_fd,
                    )
                    quarantine_target_fd = os.open(
                        quarantine.name,
                        os.O_RDONLY | nofollow | getattr(os, "O_DIRECTORY", 0),
                        dir_fd=quarantine_fd,
                    )
                    try:
                        quarantine_info = os.fstat(quarantine_target_fd)
                    finally:
                        os.close(quarantine_target_fd)
                    if (quarantine_info.st_dev, quarantine_info.st_ino) != identity:
                        raise ManagedTargetError("quarantine identity mismatch")
                    shutil.rmtree(quarantine.name, dir_fd=quarantine_fd)
                finally:
                    os.close(target_fd)
                    with contextlib.suppress(UnboundLocalError):
                        os.close(quarantine_fd)
                self._record_path(key).unlink()
                return {"id": key, "status": "deleted", "quarantine": quarantine.name}
        except (OSError, ManagedTargetError) as error:
            return {"id": key, "status": "protected", "reason": str(error)}
