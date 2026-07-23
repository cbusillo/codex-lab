#!/usr/bin/env python3
"""Prove that Codex Lab launches an app-server from its embedded CLI."""

import argparse
import ctypes
import ctypes.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from typing import Any, Callable


PROVENANCE_FIELDS = (
    "schema_version",
    "version",
    "source_commit",
    "dirty_state",
    "build_profile",
    "build_channel",
)
SELECTED_APP_PREFIX = "Selected OpenAI coding desktop app: "
SOURCE_COMMIT_PATTERN = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app_dir", type=Path)
    parser.add_argument("--timeout-seconds", type=float, default=20.0)
    args = parser.parse_args()
    print(
        json.dumps(
            run_live_smoke(args.app_dir.resolve(), args.timeout_seconds),
            indent=2,
            sort_keys=True,
        )
    )


def run_live_smoke(app_dir: Path, timeout_seconds: float) -> dict[str, Any]:
    if sys.platform != "darwin":
        raise RuntimeError("live Codex Lab desktop smoke requires macOS")
    if timeout_seconds <= 0:
        raise ValueError("timeout seconds must be greater than zero")

    launcher = app_dir / "Contents/MacOS/Codex Lab Launcher"
    cli_path = app_dir / "Contents/Resources/codex-lab"
    for label, path in (("launcher", launcher), ("embedded CLI", cli_path)):
        if not path.is_file():
            raise FileNotFoundError(f"Codex Lab {label} does not exist: {path}")

    provenance = read_cli_provenance(cli_path)
    before_rows = read_process_rows()
    before_pids = {pid for pid, _ppid, _command in before_rows}
    existing_servers = set(matching_app_server_pids(before_rows, cli_path))
    launch = subprocess.run([str(launcher)], capture_output=True, text=True, timeout=20)
    if launch.returncode != 0:
        raise RuntimeError(
            "Codex Lab launcher failed: "
            + (launch.stderr.strip() or "no diagnostic output")
        )

    selected_app = Path(selected_app_from_launcher_output(launch.stderr)).resolve()
    gui_executable = official_app_executable_path(selected_app)
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        rows = read_process_rows()
        gui_pids = {
            pid
            for pid, _ppid, _command in rows
            if pid not in before_pids and process_executable_path(pid) == gui_executable
        }
        for pid in matching_app_server_pids(rows, cli_path):
            if pid not in existing_servers and process_has_ancestor(
                pid, gui_pids, rows
            ):
                return {
                    "appServerExecutablePath": str(cli_path.resolve()),
                    "appServerPid": pid,
                    "guiPids": sorted(gui_pids),
                    "officialAppPath": str(selected_app),
                    "provenance": provenance,
                    "schemaVersion": 1,
                }
        time.sleep(0.2)
    raise TimeoutError(f"timed out waiting for an app-server using {cli_path}")


def read_cli_provenance(cli_path: Path) -> dict[str, Any]:
    completed = subprocess.run(
        [str(cli_path), "debug", "provenance", "--json"],
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    )
    if len(completed.stdout.encode()) > 4096:
        raise ValueError("Codex Lab CLI provenance exceeded the 4096-byte limit")
    try:
        provenance = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise ValueError("Codex Lab CLI emitted invalid provenance JSON") from exc
    validate_cli_provenance(provenance, cli_path)
    return {field: provenance[field] for field in PROVENANCE_FIELDS}


def validate_cli_provenance(provenance: Any, cli_path: Path) -> None:
    if not isinstance(provenance, dict) or provenance.get("schema_version") != 1:
        raise ValueError("Codex Lab CLI provenance schema is unsupported")
    source_commit = provenance.get("source_commit")
    if not isinstance(source_commit, str) or not SOURCE_COMMIT_PATTERN.fullmatch(
        source_commit
    ):
        raise ValueError("Codex Lab CLI source commit is unavailable or malformed")
    if provenance.get("dirty_state") != "clean":
        raise ValueError("Codex Lab CLI was not built from clean tracked source")
    for field, max_length in (
        ("version", 128),
        ("build_profile", 64),
        ("build_channel", 64),
    ):
        value = provenance.get(field)
        if not isinstance(value, str) or not value or len(value) > max_length:
            raise ValueError(f"Codex Lab CLI {field} is unavailable or unbounded")
    executable_path = provenance.get("executable_path")
    if (
        not isinstance(executable_path, str)
        or Path(executable_path).resolve() != cli_path.resolve()
    ):
        raise ValueError("Codex Lab CLI provenance does not match the embedded binary")


def read_process_rows() -> list[tuple[int, int, str]]:
    output = subprocess.check_output(
        ["/bin/ps", "-axo", "pid=,ppid=,command="], text=True
    )
    rows = []
    for line in output.splitlines():
        fields = line.strip().split(maxsplit=2)
        try:
            if len(fields) == 3:
                rows.append((int(fields[0]), int(fields[1]), fields[2]))
        except ValueError:
            continue
    return rows


def matching_app_server_pids(
    rows: list[tuple[int, int, str]],
    cli_path: Path,
    resolver: Callable[[int], Path | None] | None = None,
) -> list[int]:
    executable_path = resolver or process_executable_path
    expected = cli_path.resolve()
    return [
        pid
        for pid, _ppid, command in rows
        if "app-server" in command.split() and executable_path(pid) == expected
    ]


def process_executable_path(pid: int) -> Path | None:
    try:
        libproc = ctypes.CDLL(
            ctypes.util.find_library("proc") or "/usr/lib/libproc.dylib"
        )
    except OSError:
        return None
    libproc.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    libproc.proc_pidpath.restype = ctypes.c_int
    buffer = ctypes.create_string_buffer(4096)
    if libproc.proc_pidpath(pid, buffer, len(buffer)) <= 0:
        return None
    return Path(os.fsdecode(buffer.value)).resolve()


def official_app_executable_path(app_path: Path) -> Path:
    executable = subprocess.check_output(
        [
            "/usr/bin/plutil",
            "-extract",
            "CFBundleExecutable",
            "raw",
            "-o",
            "-",
            str(app_path / "Contents/Info.plist"),
        ],
        text=True,
    ).strip()
    if not executable:
        raise ValueError(f"official app has no CFBundleExecutable: {app_path}")
    return (app_path / "Contents/MacOS" / executable).resolve()


def process_has_ancestor(
    pid: int, ancestor_pids: set[int], rows: list[tuple[int, int, str]]
) -> bool:
    parents = {process_pid: ppid for process_pid, ppid, _command in rows}
    visited = set()
    while pid > 1 and pid not in visited:
        if pid in ancestor_pids:
            return True
        visited.add(pid)
        pid = parents.get(pid, 0)
    return False


def selected_app_from_launcher_output(stderr: str) -> str:
    for line in stderr.splitlines():
        if line.startswith(SELECTED_APP_PREFIX):
            selected = line.removeprefix(SELECTED_APP_PREFIX).strip()
            if selected and len(selected) <= 4096:
                return selected
    raise ValueError("Codex Lab launcher did not report the selected official app path")


if __name__ == "__main__":
    main()
