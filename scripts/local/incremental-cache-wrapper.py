#!/usr/bin/env python3
"""Route incremental rustc calls around sccache for an opt-in pilot.

Cargo invokes RUSTC_WRAPPER as ``wrapper rustc [args...]``.  Set
CODEX_LAB_SCCACHE_EXECUTABLE to select a cache executable; otherwise ``sccache``
is resolved from PATH.  The wrapper is intentionally stateless and does not
change Cargo, target, or sccache configuration.  This pilot supports the
RUSTC_WRAPPER slot only; do not combine it with RUSTC_WORKSPACE_WRAPPER.
"""

import os
import shutil
import sys
from pathlib import Path
from typing import Sequence


CACHE_EXECUTABLE_ENV = "CODEX_LAB_SCCACHE_EXECUTABLE"
WRAPPER_PATH = Path(__file__).resolve()


def _is_incremental_codegen(value: str) -> bool:
    return value == "incremental" or value.startswith("incremental=")


def has_incremental_codegen(arguments: Sequence[str]) -> bool:
    """Return whether rustc arguments request incremental code generation."""
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == "--":
            break
        if argument in {"-C", "--codegen"}:
            if index + 1 < len(arguments) and _is_incremental_codegen(
                arguments[index + 1]
            ):
                return True
            index += 2
            continue
        if argument.startswith("-C") and _is_incremental_codegen(argument[2:]):
            return True
        if argument.startswith("--codegen=") and _is_incremental_codegen(
            argument[len("--codegen=") :]
        ):
            return True
        index += 1
    return False


def _same_executable(left: str, right: str) -> bool:
    try:
        return os.path.realpath(left) == os.path.realpath(right)
    except OSError:
        return False


def _cache_executable() -> str | None:
    configured = os.environ.get(CACHE_EXECUTABLE_ENV)
    if configured is not None:
        return None if not configured else shutil.which(configured) or configured
    return shutil.which("sccache")


def _exec(
    command: list[str], description: str, *, clear_cargo_incremental: bool = False
) -> int:
    environment = None
    if clear_cargo_incremental:
        environment = os.environ.copy()
        environment.pop("CARGO_INCREMENTAL", None)
    try:
        if environment is None:
            os.execvp(command[0], command)
        else:
            os.execvpe(command[0], command, environment)
    except OSError as error:
        print(
            f"incremental-cache-wrapper: could not execute {description}: {error}",
            file=sys.stderr,
        )
        return 127
    return 127


def main(argv: Sequence[str] = sys.argv) -> int:
    if os.environ.get("RUSTC_WORKSPACE_WRAPPER"):
        print(
            "incremental-cache-wrapper: RUSTC_WORKSPACE_WRAPPER is unsupported; "
            "configure this pilot in RUSTC_WRAPPER only",
            file=sys.stderr,
        )
        return 2
    if len(argv) < 2:
        print(
            "incremental-cache-wrapper: Cargo must provide rustc as argv[1]",
            file=sys.stderr,
        )
        return 2

    compiler = argv[1]
    compiler_arguments = list(argv[1:])
    if _same_executable(compiler, str(WRAPPER_PATH)):
        print(
            "incremental-cache-wrapper: nested RUSTC_WORKSPACE_WRAPPER use is "
            "unsupported; configure this wrapper in one Cargo wrapper slot",
            file=sys.stderr,
        )
        return 2
    incremental = has_incremental_codegen(compiler_arguments[1:]) or any(
        argument.startswith("@") for argument in compiler_arguments[1:]
    )
    if incremental:
        if Path(compiler).stem.lower() == "sccache":
            if len(compiler_arguments) < 2:
                print(
                    "incremental-cache-wrapper: sccache invocation has no compiler",
                    file=sys.stderr,
                )
                return 2
            return _exec(compiler_arguments[1:], compiler_arguments[1])
        return _exec(compiler_arguments, compiler)
    if Path(compiler).stem.lower() == "sccache":
        return _exec(compiler_arguments, compiler, clear_cargo_incremental=True)

    cache = _cache_executable()
    if cache is None:
        print(
            "incremental-cache-wrapper: sccache is required for non-incremental "
            f"rustc calls; set {CACHE_EXECUTABLE_ENV} or install sccache",
            file=sys.stderr,
        )
        return 127
    if _same_executable(cache, str(WRAPPER_PATH)):
        print(
            f"incremental-cache-wrapper: {CACHE_EXECUTABLE_ENV} points to this wrapper",
            file=sys.stderr,
        )
        return 2
    return _exec([cache, *compiler_arguments], cache, clear_cargo_incremental=True)


if __name__ == "__main__":
    raise SystemExit(main())
