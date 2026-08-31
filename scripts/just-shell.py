#!/usr/bin/env python3
"""Cross-platform shell launcher for `just` recipes.

This keeps recipe bodies as normal shell snippets while giving the justfile one
portable placeholder, `{args}`, for forwarding variadic recipe arguments.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path


ARGS_TOKEN = "{args}"
STDERR_NULL_TOKEN = "{stderr-null}"
POWERSHELL_ARGS = "@($args | Select-Object -Skip 1)"
POWERSHELL_STDERR_NULL = "2>$null; exit $LASTEXITCODE"
SH_ARGS = '"$@"'
SH_STDERR_NULL = "2>/dev/null"
CARGO_ENV_RECIPES = {
    "app-server-test-client",
    "bench",
    "bench-smoke",
    "clippy",
    "code-mode-host",
    "codex",
    "exec",
    "file-search",
    "fix",
    "install",
    "log",
    "mcp-server-run",
    "test",
    "tui-with-exec-server",
    "write-config-schema",
    "write-hooks-schema",
}
V8_ENV_RECIPES = {"bench", "bench-smoke", "clippy", "code-mode-host", "fix", "test"}


def main() -> int:
    if len(sys.argv) < 2:
        print("just shell adapter expected a recipe command.", file=sys.stderr)
        return 1

    command = sys.argv[1]
    recipe_name = sys.argv[2] if len(sys.argv) > 2 else ""
    recipe_args = sys.argv[3:]

    if os.name == "nt":
        return run_powershell(command, recipe_name, recipe_args)
    else:
        return run_sh(command, recipe_name, recipe_args)


def run_sh(command: str, recipe_name: str, recipe_args: list[str]) -> int:
    os.environ.update(resolve_cargo_environment(recipe_name, os.environ))
    os.environ.update(resolve_rusty_v8_environment(recipe_name, os.environ))
    command = command.replace(ARGS_TOKEN, SH_ARGS)
    command = command.replace(STDERR_NULL_TOKEN, SH_STDERR_NULL)
    os.execvp("sh", ["sh", "-cu", command, recipe_name, *recipe_args])


def resolve_cargo_environment(
    recipe_name: str, environment: Mapping[str, str]
) -> dict[str, str]:
    if recipe_name not in CARGO_ENV_RECIPES:
        return {}
    repo_root = environment.get("CODEX_REPO_ROOT")
    if not repo_root:
        return {}
    resolver = Path(repo_root) / "scripts" / "local" / "cargo-build-env.sh"
    completed = subprocess.run(
        [str(resolver)],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=None,
        env=dict(environment),
    )
    if completed.returncode != 0:
        raise SystemExit(completed.returncode)
    return {"CARGO_TARGET_DIR": completed.stdout.strip()}


def resolve_rusty_v8_environment(
    recipe_name: str, environment: Mapping[str, str]
) -> dict[str, str]:
    if recipe_name not in V8_ENV_RECIPES:
        return {}
    archive = environment.get("RUSTY_V8_ARCHIVE")
    binding = environment.get("RUSTY_V8_SRC_BINDING_PATH")
    if bool(archive) != bool(binding):
        print(
            "both RUSTY_V8_ARCHIVE and RUSTY_V8_SRC_BINDING_PATH must be set together",
            file=sys.stderr,
        )
        raise SystemExit(2)
    if archive and binding:
        return {}
    repo_root = environment.get("CODEX_REPO_ROOT")
    if not repo_root:
        return {}
    helper = Path(repo_root) / "scripts" / "local" / "rusty_v8_env.py"
    completed = subprocess.run(
        [sys.executable, str(helper), "resolve"],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=None,
        env=dict(environment),
    )
    if completed.returncode != 0:
        raise SystemExit(completed.returncode)
    exports: dict[str, str] = {}
    for line in completed.stdout.splitlines():
        key, separator, value = line.partition("=")
        if separator and key in {"RUSTY_V8_ARCHIVE", "RUSTY_V8_SRC_BINDING_PATH"}:
            exports[key] = value
    return exports


def run_powershell(command: str, recipe_name: str, recipe_args: list[str]) -> int:
    pwsh = shutil.which("pwsh.exe") or shutil.which("pwsh")
    if pwsh is None:
        print(
            "PowerShell ('pwsh') is required for Windows just recipes. "
            "Run 'just install' to install it.",
            file=sys.stderr,
        )
        return 1

    command = command.replace(ARGS_TOKEN, POWERSHELL_ARGS)
    command = command.replace(STDERR_NULL_TOKEN, POWERSHELL_STDERR_NULL)
    return subprocess.run(
        [
            pwsh,
            "-NoLogo",
            "-NoProfile",
            "-CommandWithArgs",
            command,
            recipe_name,
            *recipe_args,
        ],
        check=False,
    ).returncode


if __name__ == "__main__":
    raise SystemExit(main())
