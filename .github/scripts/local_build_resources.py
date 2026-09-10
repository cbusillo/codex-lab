#!/usr/bin/env python3

"""Validate and apply opt-in resource controls for local Codex Lab builds.

The profile requires CODEX_LOCAL_BUILD_JOBS, CODEX_LOCAL_BUILD_MEMORY_MB,
CODEX_LOCAL_BUILD_LOCK, and a matching CARGO_BUILD_JOBS. Its durable flock file
coordinates non-nested leaf commands on same-user runners; it is not ownership
state and is never deleted. The supervisor forwards ordinary cancellation and
holds the lock through child exit, but SIGKILL cannot be forwarded and could
leave an orphan after the operating system releases the supervisor's lock.
"""

import argparse
import os
import signal
import subprocess
import sys
from collections.abc import Mapping
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path


PROFILE_ENV_NAMES = (
    "CODEX_LOCAL_BUILD_JOBS",
    "CODEX_LOCAL_BUILD_MEMORY_MB",
    "CODEX_LOCAL_BUILD_LOCK",
)
REMOTE_EXECUTION_CONFIGS = {
    "--config=ci-linux",
    "--config=ci-macos",
    "--config=ci-v8",
    "--config=ci-windows-cross",
}
BUILD_COMMANDS = {"build", "coverage", "run", "test"}


class ResourceProfileError(ValueError):
    """The runner supplied an incomplete or unsafe local build profile."""


@dataclass(frozen=True)
class LocalBuildResources:
    jobs: int
    memory_mb: int
    lock_path: Path

    @classmethod
    def from_env(cls, env: Mapping[str, str]) -> "LocalBuildResources | None":
        present_names = [name for name in PROFILE_ENV_NAMES if name in env]
        if not present_names:
            return None

        missing_names = [name for name in PROFILE_ENV_NAMES if not env.get(name)]
        if missing_names:
            raise ResourceProfileError(
                "local build resource profile is incomplete; missing "
                + ", ".join(missing_names)
            )

        expected_identity = {
            "GITHUB_ACTIONS": "true",
            "RUNNER_ENVIRONMENT": "self-hosted",
            "RUNNER_OS": "macOS",
        }
        identity_errors = [
            f"{name}={env.get(name, '')!r} (expected {expected!r})"
            for name, expected in expected_identity.items()
            if env.get(name) != expected
        ]
        if identity_errors:
            raise ResourceProfileError(
                "local build resource profile requires a self-hosted macOS "
                "GitHub Actions runner; " + "; ".join(identity_errors)
            )

        jobs = _positive_integer(
            env["CODEX_LOCAL_BUILD_JOBS"], "CODEX_LOCAL_BUILD_JOBS"
        )
        memory_mb = _positive_integer(
            env["CODEX_LOCAL_BUILD_MEMORY_MB"], "CODEX_LOCAL_BUILD_MEMORY_MB"
        )
        cargo_jobs = env.get("CARGO_BUILD_JOBS")
        if not cargo_jobs:
            raise ResourceProfileError(
                "local build resource profile is incomplete; missing CARGO_BUILD_JOBS"
            )
        parsed_cargo_jobs = _positive_integer(cargo_jobs, "CARGO_BUILD_JOBS")
        if parsed_cargo_jobs != jobs:
            raise ResourceProfileError(
                "CARGO_BUILD_JOBS must match CODEX_LOCAL_BUILD_JOBS "
                f"({parsed_cargo_jobs} != {jobs})"
            )

        lock_path = Path(env["CODEX_LOCAL_BUILD_LOCK"])
        if not lock_path.is_absolute():
            raise ResourceProfileError(
                "CODEX_LOCAL_BUILD_LOCK must be an absolute path"
            )
        if lock_path.name in {"", ".", ".."}:
            raise ResourceProfileError("CODEX_LOCAL_BUILD_LOCK must name a lock file")

        return cls(jobs=jobs, memory_mb=memory_mb, lock_path=lock_path)

    def bazel_args(self) -> list[str]:
        """Return scheduler estimates, not hard process or memory limits."""
        return [
            f"--jobs={self.jobs}",
            f"--local_resources=cpu={self.jobs}",
            f"--local_resources=memory={self.memory_mb}",
        ]


def _positive_integer(value: str, name: str) -> int:
    if not value.isascii() or not value.isdecimal() or value.startswith("0"):
        raise ResourceProfileError(f"{name} must be a positive base-10 integer")
    parsed = int(value)
    if parsed <= 0:
        raise ResourceProfileError(f"{name} must be a positive base-10 integer")
    return parsed


def bazel_command(args: Sequence[str]) -> str | None:
    return next((arg for arg in args if not arg.startswith("-")), None)


def bazel_uses_remote_execution(args: Sequence[str], env: Mapping[str, str]) -> bool:
    if not env.get("BUILDBUDDY_API_KEY"):
        return False
    try:
        separator_idx = args.index("--")
    except ValueError:
        separator_idx = len(args)
    return any(arg in REMOTE_EXECUTION_CONFIGS for arg in args[:separator_idx])


def bazel_resource_args(args: Sequence[str], env: Mapping[str, str]) -> list[str]:
    profile = LocalBuildResources.from_env(env)
    if (
        profile is None
        or bazel_uses_remote_execution(args, env)
        or bazel_command(args) not in BUILD_COMMANDS
    ):
        return []
    return profile.bazel_args()


def normalized_exit_code(return_code: int) -> int:
    if return_code < 0:
        return 128 - return_code
    return return_code


def run_with_optional_lock(
    command: Sequence[str],
    *,
    env: Mapping[str, str] | None = None,
    use_lock: bool = True,
) -> int:
    if not command:
        raise ResourceProfileError("expected a command after --")
    source_env = os.environ if env is None else env
    profile = LocalBuildResources.from_env(source_env)
    if profile is None or not use_lock:
        os.execvpe(command[0], list(command), dict(source_env))
    if sys.platform != "darwin":
        raise ResourceProfileError(
            "local build locking is supported only on a macOS host"
        )

    profile.lock_path.parent.mkdir(parents=True, exist_ok=True)
    lock_fd = os.open(profile.lock_path, os.O_CREAT | os.O_RDWR, 0o600)
    try:
        # Import lazily: this path is macOS-only, while the Bazel wrapper also
        # runs on Windows where fcntl is unavailable.
        import fcntl

        print(
            f"Waiting for shared Codex Lab local build lock: {profile.lock_path}",
            file=sys.stderr,
            flush=True,
        )
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        print(
            f"Acquired shared Codex Lab local build lock: {profile.lock_path}",
            file=sys.stderr,
            flush=True,
        )
        # Keep the descriptor in this short-lived supervisor. Bazel servers can
        # outlive their clients, so inheriting it could retain the lock after
        # the requested invocation has completed.
        os.set_inheritable(lock_fd, False)
        child: subprocess.Popen[bytes] | None = None
        pending_signals: list[int] = []

        def forward_signal(received_signal: int, _frame: object) -> None:
            if child is None:
                pending_signals.append(received_signal)
            elif child.poll() is None:
                child.send_signal(received_signal)

        previous_handlers = {
            signal_number: signal.signal(signal_number, forward_signal)
            for signal_number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
        }
        try:
            if pending_signals:
                return normalized_exit_code(-pending_signals[0])
            spawned_child = subprocess.Popen(
                list(command), env=dict(source_env), close_fds=True
            )
            child = spawned_child
            for pending_signal in pending_signals:
                spawned_child.send_signal(pending_signal)
            return normalized_exit_code(spawned_child.wait())
        finally:
            for signal_number, previous_handler in previous_handlers.items():
                signal.signal(signal_number, previous_handler)
    finally:
        os.close(lock_fd)


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="subcommand", required=True)
    bazel_parser = subparsers.add_parser(
        "bazel-args", help="print local Bazel resource flags, one per line"
    )
    bazel_parser.add_argument("args", nargs=argparse.REMAINDER)
    exec_parser = subparsers.add_parser(
        "exec", help="run a heavy local command under the shared build lock"
    )
    exec_parser.add_argument("command", nargs=argparse.REMAINDER)
    return parser.parse_args(argv)


def main(argv: Sequence[str]) -> int:
    args = parse_args(argv)
    if args.subcommand == "bazel-args":
        bazel_args = args.args[1:] if args.args[:1] == ["--"] else args.args
        for arg in bazel_resource_args(bazel_args, os.environ):
            print(arg)
        return 0

    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    return run_with_optional_lock(command)


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except ResourceProfileError as error:
        print(f"Invalid local build resource profile: {error}", file=sys.stderr)
        raise SystemExit(2) from error
