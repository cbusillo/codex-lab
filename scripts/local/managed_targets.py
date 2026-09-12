#!/usr/bin/env python3
"""Opt in to managed Cargo target leases for supported local recipes."""

import argparse
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from collections.abc import Sequence
from typing import Protocol


CONFIG_ENV = "CODEX_LAB_TARGET_RETENTION_CONFIG"
DEFAULT_CONFIG_PATH = Path.home() / ".config/codex-lab/target-retention.json"
LEASE_ENV = "CODEX_LAB_TARGET_LEASE_FD"
TARGET_OVERRIDE_ENV_NAMES = (
    "CARGO_TARGET_DIR",
    "CODEX_LAB_CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
)
AUTO_ENROLL_RECIPES = frozenset({"test", "clippy", "fix", "assemble-codex-package"})
MANAGED_RECIPES = AUTO_ENROLL_RECIPES | {"build"}
COMMON_VALUE_FLAGS = frozenset(
    {"-p", "--package", "--bin", "--example", "--features", "--profile", "--target"}
)
COMMON_BOOL_FLAGS = frozenset(
    {
        "--all-features",
        "--all-targets",
        "--locked",
        "--no-default-features",
        "--offline",
        "--release",
        "--workspace",
    }
)
TEST_VALUE_FLAGS = COMMON_VALUE_FLAGS | {
    "-E",
    "--filter-expr",
    "--partition",
    "--retries",
    "--test",
    "--test-threads",
}
TEST_BOOL_FLAGS = COMMON_BOOL_FLAGS | {
    "--benches",
    "--examples",
    "--lib",
    "--no-fail-fast",
    "--tests",
}
PACKAGE_VALUE_FLAGS = frozenset(
    {
        "--archive-output",
        "--cargo-profile",
        "--package-dir",
        "--package-version",
        "--target",
        "--variant",
    }
)
PACKAGE_BOOL_FLAGS = frozenset({"--force", "--overwrite-archives"})
DEFAULT_MAX_AGE_DAYS = 14
DEFAULT_MIN_IDLE_HOURS = 24
DEFAULT_MIN_FREE_BYTES = 200 * 1024**3
DEFAULT_MAX_TARGETS = 1
MAX_CONFIG_BYTES = 1024 * 1024
KACHE_STATUS_TIMEOUT_SECONDS = 10
KACHE_GC_TIMEOUT_SECONDS = 30


class ManagedTargetsError(RuntimeError):
    """Raised when managed target enrollment cannot be proven safe."""


class ManagedStore(Protocol):
    def initialize(self) -> None: ...

    def collect(self, apply: bool = False) -> dict[str, object]: ...

    def run(self, worktree: Path, command: list[str], cwd: Path) -> int: ...


def config_path(value: str | None = None) -> tuple[Path, bool]:
    """Return the configured path and whether the caller explicitly selected it."""

    if value is not None:
        return Path(value).expanduser(), True
    configured = os.environ.get(CONFIG_ENV)
    if configured is not None:
        return Path(configured).expanduser(), True
    return DEFAULT_CONFIG_PATH, False


def _config_path_without_symlinks(path: Path) -> Path:
    path = path.absolute()
    current = Path(path.anchor)
    for component in path.parts[1:]:
        current /= component
        try:
            info = os.lstat(current)
        except FileNotFoundError:
            break
        except OSError as error:
            raise ManagedTargetsError(
                f"cannot inspect target retention config path {path}: {error}"
            ) from error
        if stat.S_ISLNK(info.st_mode):
            raise ManagedTargetsError(
                f"target retention config path contains a symlink: {current}"
            )
    return path


def existing_config_path(value: str | None = None) -> Path | None:
    path, explicit = config_path(value)
    path = _config_path_without_symlinks(path)
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        info = None
    except OSError as error:
        raise ManagedTargetsError(
            f"cannot inspect target retention config {path}: {error}"
        ) from error
    if info is not None:
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            raise ManagedTargetsError(
                f"target retention config is not a regular file: {path}"
            )
        return path
    if explicit:
        raise ManagedTargetsError(
            f"configured target retention file is missing: {path}"
        )
    return None


def load_config(value: str | None = None) -> tuple[dict[str, object], Path]:
    path = existing_config_path(value)
    if path is None:
        raise ManagedTargetsError(
            f"target retention is not configured; create {DEFAULT_CONFIG_PATH} with init"
        )
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_CONFIG_BYTES:
                raise ManagedTargetsError(
                    f"target retention config is invalid or oversized: {path}"
                )
            payload = json.loads(os.read(fd, MAX_CONFIG_BYTES + 1).decode("utf-8"))
        finally:
            os.close(fd)
    except ManagedTargetsError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ManagedTargetsError(
            f"cannot read target retention config {path}: {error}"
        ) from error
    if not isinstance(payload, dict):
        raise ManagedTargetsError(
            f"target retention config must be a JSON object: {path}"
        )
    return payload, path


def _store(config: dict[str, object]) -> ManagedStore:
    try:
        from managed_target_store import ManagedTargetStore
    except ImportError as error:
        raise ManagedTargetsError("managed target engine is unavailable") from error
    return ManagedTargetStore(config)


def _create_config(path: Path, config: dict[str, object]) -> None:
    path = _config_path_without_symlinks(path)
    encoded = (json.dumps(config, indent=2, sort_keys=True) + "\n").encode("utf-8")
    if path.exists() or path.is_symlink():
        raise ManagedTargetsError(f"refusing to overwrite existing config: {path}")
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
        temporary = Path(temporary_name)
        try:
            os.fchmod(fd, 0o600)
            with os.fdopen(fd, "wb") as stream:
                stream.write(encoded)
                stream.flush()
                os.fsync(stream.fileno())
            os.link(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)
    except FileExistsError as error:
        raise ManagedTargetsError(
            f"refusing to overwrite existing config: {path}"
        ) from error
    except (OSError, UnicodeError) as error:
        raise ManagedTargetsError(
            f"managed root initialized but config creation failed; inspect {path}: {error}"
        ) from error


def _repo_root() -> Path:
    root = Path(__file__).resolve().parents[2]
    try:
        reported = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--show-toplevel"],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout.strip()
    except (OSError, subprocess.SubprocessError) as error:
        raise ManagedTargetsError(
            f"cannot validate managed worktree Git root: {error}"
        ) from error
    if Path(reported).resolve() != root:
        raise ManagedTargetsError(f"managed worktree root mismatch: {reported}")
    return root


def _has_target_override(arguments: Sequence[str], environment: dict[str, str]) -> bool:
    if any(name in environment for name in TARGET_OVERRIDE_ENV_NAMES):
        return True
    return any(
        argument == "--target-dir"
        or argument.startswith("--target-dir=")
        or argument == "--manifest-path"
        or argument.startswith("--manifest-path=")
        or argument == "--config"
        or argument.startswith("--config=")
        for argument in arguments
    )


def _override_paths(arguments: Sequence[str], environment: dict[str, str]) -> list[str]:
    paths = [
        environment[name] for name in TARGET_OVERRIDE_ENV_NAMES if name in environment
    ]
    for index, argument in enumerate(arguments):
        if argument in {
            "--target-dir",
            "--manifest-path",
            "--config",
        } and index + 1 < len(arguments):
            paths.append(arguments[index + 1])
        elif (
            argument.startswith("--target-dir=")
            or argument.startswith("--manifest-path=")
            or argument.startswith("--config=")
        ):
            paths.append(argument.split("=", 1)[1])
    return paths


def _inside(path: str, root: Path) -> bool:
    try:
        Path(path).expanduser().resolve(strict=False).relative_to(
            root.expanduser().resolve(strict=False)
        )
    except ValueError:
        return False
    except (OSError, RuntimeError) as error:
        raise ManagedTargetsError(
            f"cannot resolve target override path {path!r}: {error}"
        ) from error
    return True


def reject_managed_override(
    config: dict[str, object], arguments: Sequence[str], environment: dict[str, str]
) -> None:
    managed_root = config.get("managed_root")
    if not isinstance(managed_root, str):
        return
    if any(
        _inside(path, Path(managed_root))
        for path in _override_paths(arguments, environment)
    ):
        raise ManagedTargetsError(
            "explicit target override points inside the managed target root"
        )


def _valid_argument_value(value: str) -> bool:
    return bool(value) and "\x00" not in value and not value.startswith("-")


def _safe_arguments(
    arguments: Sequence[str], value_flags: frozenset[str], bool_flags: frozenset[str]
) -> bool:
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == "--":
            return False
        if argument in {"--target-dir", "--manifest-path", "--config"}:
            return False
        if argument.startswith("--"):
            name, separator, value = argument.partition("=")
            if name in bool_flags and not separator:
                index += 1
                continue
            if name not in value_flags:
                return False
            if separator:
                if not _valid_argument_value(value):
                    return False
            elif index + 1 >= len(arguments) or not _valid_argument_value(
                arguments[index + 1]
            ):
                return False
            else:
                index += 1
        elif argument in bool_flags:
            pass
        elif argument in value_flags:
            if index + 1 >= len(arguments) or not _valid_argument_value(
                arguments[index + 1]
            ):
                return False
            index += 1
        elif argument.startswith("-"):
            return False
        elif "\x00" in argument:
            return False
        index += 1
    return True


def supported_recipe_arguments(recipe: str, arguments: Sequence[str]) -> bool:
    if recipe in {"test", "build", "clippy", "fix"}:
        value_flags = TEST_VALUE_FLAGS if recipe == "test" else COMMON_VALUE_FLAGS
        bool_flags = TEST_BOOL_FLAGS if recipe == "test" else COMMON_BOOL_FLAGS
        return _safe_arguments(arguments, value_flags, bool_flags)
    if recipe == "assemble-codex-package":
        return _safe_arguments(arguments, PACKAGE_VALUE_FLAGS, PACKAGE_BOOL_FLAGS)
    return False


def validate_package_outputs(
    config: dict[str, object], arguments: Sequence[str]
) -> None:
    managed_root = config.get("managed_root")
    if not isinstance(managed_root, str):
        raise ManagedTargetsError("managed target config has no managed_root")
    if not any(
        argument == "--package-dir" or argument.startswith("--package-dir=")
        for argument in arguments
    ) and _inside(tempfile.gettempdir(), Path(managed_root)):
        raise ManagedTargetsError(
            "default package output points inside the managed target root"
        )
    for index, argument in enumerate(arguments):
        if argument in {"--package-dir", "--archive-output"} and index + 1 < len(
            arguments
        ):
            value = arguments[index + 1]
        elif argument.startswith("--package-dir=") or argument.startswith(
            "--archive-output="
        ):
            value = argument.split("=", 1)[1]
        else:
            continue
        if _inside(value, Path(managed_root)):
            raise ManagedTargetsError(
                "package output points inside the managed target root"
            )


def managed_recipe_available(
    recipe: str,
    arguments: Sequence[str],
    environment: dict[str, str] | None = None,
) -> bool:
    """Return whether this invocation may enroll in the managed engine."""

    source = dict(os.environ) if environment is None else environment
    return (
        recipe in MANAGED_RECIPES
        and LEASE_ENV not in source
        and not _has_target_override(arguments, source)
        and supported_recipe_arguments(recipe, arguments)
    )


def recipe_command(
    root: Path, recipe: str, arguments: Sequence[str]
) -> tuple[list[str], Path]:
    just = shutil.which("just") or "just"
    if recipe == "test":
        return [
            just,
            "--justfile",
            str(root / "justfile"),
            "test",
            *arguments,
        ], root / "codex-rs"
    if recipe == "clippy":
        return [
            just,
            "--justfile",
            str(root / "justfile"),
            "clippy",
            *arguments,
        ], root / "codex-rs"
    if recipe == "fix":
        return [
            just,
            "--justfile",
            str(root / "justfile"),
            "fix",
            *arguments,
        ], root / "codex-rs"
    if recipe == "assemble-codex-package":
        return [
            just,
            "--justfile",
            str(root / "justfile"),
            "assemble-codex-package",
            *arguments,
        ], root / "codex-rs"
    if recipe == "build":
        return ["cargo", "build", *arguments], root / "codex-rs"
    raise ManagedTargetsError(f"recipe is outside the managed allowlist: {recipe}")


def _kache_status(kache: str) -> dict[str, object]:
    try:
        completed = subprocess.run(
            [kache, "daemon", "status", "--json"],
            check=False,
            capture_output=True,
            text=True,
            timeout=KACHE_STATUS_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise ManagedTargetsError(f"Kache daemon status failed: {error}") from error
    if completed.returncode != 0:
        raise ManagedTargetsError(
            f"Kache daemon status failed (exit {completed.returncode}): "
            f"{completed.stderr.strip() or completed.stdout.strip()}"
        )
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise ManagedTargetsError(
            f"Kache daemon status was not JSON: {error}"
        ) from error
    if not isinstance(payload, dict):
        raise ManagedTargetsError("Kache daemon status was not a JSON object")
    return payload


def prewarm_kache(config: dict[str, object]) -> None:
    kache_enabled = config.get("kache_gc", True)
    if isinstance(kache_enabled, bool) and not kache_enabled:
        return
    kache = shutil.which("kache")
    if kache is None:
        return
    status = _kache_status(kache)
    client_version = status.get("version")
    if not isinstance(client_version, str) or not client_version:
        raise ManagedTargetsError("Kache daemon status has no valid client version")
    if status.get("service_executable_mismatch"):
        raise ManagedTargetsError("Kache daemon is running with a version mismatch")
    if status.get("daemon_running") and status.get("daemon_version") == client_version:
        return
    if status.get("daemon_running") and status.get("daemon_version") is not None:
        raise ManagedTargetsError("Kache daemon is running with a version mismatch")
    if not status.get("daemon_running"):
        try:
            started = subprocess.run(
                [kache, "daemon", "start"],
                check=False,
                capture_output=True,
                text=True,
                timeout=KACHE_STATUS_TIMEOUT_SECONDS,
            )
        except (OSError, subprocess.SubprocessError) as error:
            raise ManagedTargetsError(f"Kache daemon start failed: {error}") from error
        if started.returncode != 0:
            raise ManagedTargetsError(
                f"Kache daemon start failed (exit {started.returncode}): "
                f"{started.stderr.strip() or started.stdout.strip()}"
            )
    deadline = time.monotonic() + KACHE_STATUS_TIMEOUT_SECONDS
    while True:
        status = _kache_status(kache)
        if status.get("service_executable_mismatch"):
            raise ManagedTargetsError("Kache daemon started with a version mismatch")
        if (
            status.get("daemon_running")
            and status.get("daemon_version") == client_version
        ):
            return
        if status.get("daemon_running") and status.get("daemon_version") is not None:
            raise ManagedTargetsError("Kache daemon started with a version mismatch")
        if time.monotonic() >= deadline:
            raise ManagedTargetsError(
                "Kache daemon did not become ready before timeout"
            )
        time.sleep(0.1)


def _deleted_items(report: dict[str, object]) -> list[object]:
    actions = report.get("actions")
    if isinstance(actions, list):
        return [
            action
            for action in actions
            if isinstance(action, dict) and action.get("status") == "deleted"
        ]
    return []


def run_kache_gc(config: dict[str, object], report: dict[str, object]) -> bool:
    kache_enabled = config.get("kache_gc", True)
    if not _deleted_items(report) or (
        isinstance(kache_enabled, bool) and not kache_enabled
    ):
        return True
    kache = shutil.which("kache")
    if kache is None:
        return True
    try:
        completed = subprocess.run(
            [kache, "gc", "--json"],
            check=False,
            capture_output=True,
            text=True,
            timeout=KACHE_GC_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.SubprocessError) as error:
        report["kache_gc"] = {"status": "failed", "error": str(error)}
        return False
    if completed.returncode != 0:
        report["kache_gc"] = {
            "status": "failed",
            "error": (
                f"exit {completed.returncode}: "
                f"{completed.stderr.strip() or completed.stdout.strip()}"
            ),
        }
        return False
    try:
        value = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        report["kache_gc"] = {"status": "failed", "error": f"invalid JSON: {error}"}
        return False
    report["kache_gc"] = value
    return True


def _refresh_free_space(store: ManagedStore, report: dict[str, object]) -> int:
    refreshed = store.collect(apply=False)
    if not isinstance(refreshed, dict):
        raise ManagedTargetsError(
            "managed storage remeasurement did not return a JSON object"
        )
    available = refreshed.get("actual_free_bytes")
    if isinstance(available, bool) or not isinstance(available, int):
        raise ManagedTargetsError(
            "managed storage remeasurement returned invalid free space"
        )
    report["actual_free_bytes"] = available
    return available


def admit_storage(
    store: ManagedStore, config: dict[str, object], preview: dict[str, object]
) -> None:
    """Perform one pressure-triggered collection before acquiring a target lease."""

    floor = config.get("min_free_bytes", DEFAULT_MIN_FREE_BYTES)
    if isinstance(floor, bool) or not isinstance(floor, int) or floor < 0:
        raise ManagedTargetsError(
            "managed storage admission has invalid min_free_bytes"
        )
    available = preview.get("actual_free_bytes")
    if isinstance(available, bool) or not isinstance(available, int):
        raise ManagedTargetsError(
            "managed storage admission returned invalid free space"
        )
    if available >= floor:
        return
    report = store.collect(apply=True)
    if not isinstance(report, dict):
        raise ManagedTargetsError(
            "managed storage collection did not return a JSON object"
        )
    kache_ok = run_kache_gc(config, report)
    try:
        available = _refresh_free_space(store, report)
    except ManagedTargetsError:
        print(json.dumps(report, sort_keys=True), file=sys.stderr)
        raise
    if not kache_ok:
        print(json.dumps(report, sort_keys=True), file=sys.stderr)
        raise ManagedTargetsError("Kache GC failed after managed target deletion")
    if available < floor:
        if _deleted_items(report):
            print(json.dumps(report, sort_keys=True), file=sys.stderr)
        raise ManagedTargetsError(
            f"managed target admission refused: free space remains below {floor} bytes"
        )


def run_recipe(config: dict[str, object], recipe: str, arguments: Sequence[str]) -> int:
    if recipe not in MANAGED_RECIPES:
        raise ManagedTargetsError(f"recipe is outside the managed allowlist: {recipe}")
    if not supported_recipe_arguments(recipe, arguments):
        raise ManagedTargetsError(f"unsupported flags for managed recipe: {recipe}")
    if not managed_recipe_available(recipe, arguments):
        raise ManagedTargetsError(
            f"managed enrollment refused for {recipe}: explicit target/config override or inherited lease"
        )
    if recipe == "assemble-codex-package":
        validate_package_outputs(config, arguments)
    root = _repo_root()
    command, cwd = recipe_command(root, recipe, arguments)
    store = _store(config)
    preview = store.collect(apply=False)
    if not isinstance(preview, dict):
        raise ManagedTargetsError(
            "managed storage admission did not return a JSON object"
        )
    prewarm_kache(config)
    admit_storage(store, config, preview)
    return store.run(root, command, cwd)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", help="host-local target retention JSON path")
    subparsers = parser.add_subparsers(dest="subcommand", required=True)
    init = subparsers.add_parser("init")
    init.add_argument("--volume-root", required=True)
    init.add_argument("--volume-uuid", required=True)
    init.add_argument("--managed-root", required=True)
    init.add_argument("--max-age-days", type=int, default=DEFAULT_MAX_AGE_DAYS)
    init.add_argument("--min-idle-hours", type=float, default=DEFAULT_MIN_IDLE_HOURS)
    init.add_argument("--min-free-bytes", type=int, default=DEFAULT_MIN_FREE_BYTES)
    init.add_argument("--max-targets", type=int, default=DEFAULT_MAX_TARGETS)
    init.add_argument("--no-kache-gc", action="store_true")
    gc = subparsers.add_parser("gc")
    gc.add_argument("--apply", action="store_true")
    run = subparsers.add_parser("run")
    run.add_argument("--recipe", choices=sorted(MANAGED_RECIPES), required=True)
    run.add_argument("arguments", nargs=argparse.REMAINDER)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    engine_error_types: tuple[type[Exception], ...] = ()
    try:
        import managed_target_store
    except ImportError:
        pass
    else:
        engine_error_types = (managed_target_store.ManagedTargetError,)
    try:
        if args.subcommand == "init":
            path, _ = config_path(args.config)
            path = _config_path_without_symlinks(path)
            if path.exists() or path.is_symlink():
                raise ManagedTargetsError(
                    f"refusing to overwrite existing config: {path}"
                )
            config = {
                "schema": 1,
                "volume_root": args.volume_root,
                "volume_uuid": args.volume_uuid,
                "managed_root": args.managed_root,
                "max_age_days": args.max_age_days,
                "min_idle_hours": args.min_idle_hours,
                "min_free_bytes": args.min_free_bytes,
                "max_targets": args.max_targets,
                "kache_gc": not args.no_kache_gc,
            }
            store = _store(config)
            store.initialize()
            _create_config(path, config)
            print(
                json.dumps(
                    {"status": "initialized", "config": str(path)}, sort_keys=True
                )
            )
            return 0
        config, _ = load_config(args.config)
        store = _store(config)
        if args.subcommand == "gc":
            report = store.collect(apply=args.apply)
            if not isinstance(report, dict):
                raise ManagedTargetsError(
                    "managed target collection did not return a JSON object"
                )
            kache_ok = run_kache_gc(config, report)
            free_ok = True
            if _deleted_items(report):
                try:
                    _refresh_free_space(store, report)
                except ManagedTargetsError as error:
                    report["free_space_refresh"] = {
                        "status": "failed",
                        "error": str(error),
                    }
                    free_ok = False
            print(json.dumps(report, sort_keys=True))
            return 0 if kache_ok and free_ok else 2
        arguments = list(args.arguments)
        if arguments[:1] == ["--"]:
            arguments = arguments[1:]
        return run_recipe(config, args.recipe, arguments)
    except ManagedTargetsError as error:
        print(f"managed-targets: {error}", file=sys.stderr)
        return 2
    except engine_error_types as error:
        print(f"managed-targets: {error}", file=sys.stderr)
        return 2
    except (OSError, ValueError) as error:
        print(f"managed-targets: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
