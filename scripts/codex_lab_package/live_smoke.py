#!/usr/bin/env python3
"""Prove that Codex Lab launches against its persistent local daemon."""

import argparse
import ctypes
import ctypes.util
import json
import math
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
MANAGED_CLI_RELATIVE_PATH = Path("packages/standalone/current/codex")
STABILITY_WINDOW_SECONDS = 5.0
MIN_TIMEOUT_SECONDS = 10.0
MAX_DESKTOP_LOG_BYTES = 512 * 1024


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
    validate_timeout_seconds(timeout_seconds)

    launcher = app_dir / "Contents/MacOS/Codex Lab Launcher"
    cli_path = app_dir / "Contents/Resources/codex-lab"
    for label, path in (("launcher", launcher), ("embedded CLI", cli_path)):
        if not path.is_file():
            raise FileNotFoundError(f"Codex Lab {label} does not exist: {path}")

    provenance = read_cli_provenance(cli_path)
    lab_home = Path(
        os.environ.get("CODEX_LAB_HOME", Path.home() / ".codex-lab")
    ).resolve()
    managed_cli_path = lab_home / MANAGED_CLI_RELATIVE_PATH
    if not managed_cli_path.is_file():
        raise FileNotFoundError(
            f"managed Codex Lab CLI does not exist: {managed_cli_path}"
        )
    managed_provenance = read_cli_provenance(managed_cli_path)
    validate_matching_build_provenance(provenance, managed_provenance)
    before_rows = read_process_rows()
    before_pids = {pid for pid, _ppid, _command in before_rows}
    deadline = time.monotonic() + timeout_seconds
    try:
        launch = subprocess.run(
            [str(launcher)],
            capture_output=True,
            text=True,
            timeout=max(0.1, deadline - time.monotonic()),
        )
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError("timed out launching Codex Lab") from exc
    if launch.returncode != 0:
        raise RuntimeError(
            "Codex Lab launcher failed: "
            + (launch.stderr.strip() or "no diagnostic output")
        )

    selected_app = Path(selected_app_from_launcher_output(launch.stderr)).resolve()
    gui_executable = official_app_executable_path(selected_app)
    expected_environment = {
        "CODEX_APP_SERVER_USE_LOCAL_DAEMON": "1",
        "CODEX_APP_SERVER_FORCE_CLI": "",
        "CODEX_CLI_PATH": "",
        "CODEX_HOME": str(lab_home),
        "CODEX_LAB_HOME": str(lab_home),
    }
    stable_since = None
    while time.monotonic() < deadline:
        rows = read_process_rows()
        gui_pids = {
            pid
            for pid, _ppid, _command in rows
            if pid not in before_pids and process_executable_path(pid) == gui_executable
        }
        matching_gui_pids = sorted(
            pid
            for pid in gui_pids
            if process_has_environment(pid, expected_environment)
        )
        new_gui_app_servers = [
            pid
            for pid, _ppid, command in rows
            if pid not in before_pids
            and is_serving_app_server_command(command)
            and process_has_ancestor(pid, gui_pids, rows)
        ]
        if new_gui_app_servers:
            raise RuntimeError(
                "official app launched a bundled stdio app-server instead of the persistent daemon"
            )
        managed_servers = sorted(matching_app_server_pids(rows, managed_cli_path))
        transport_proof = desktop_transport_proof(matching_gui_pids)
        if matching_gui_pids and managed_servers and transport_proof is not None:
            stable_since = stable_since or time.monotonic()
            if time.monotonic() - stable_since >= STABILITY_WINDOW_SECONDS:
                return {
                    "appServerExecutablePath": str(managed_cli_path.resolve()),
                    "appServerPid": managed_servers[0],
                    **transport_proof,
                    "guiPids": matching_gui_pids,
                    "managedProvenance": managed_provenance,
                    "mode": "persistentLocalDaemon",
                    "officialAppPath": str(selected_app),
                    "provenance": provenance,
                    "schemaVersion": 1,
                    "stabilityWindowSeconds": STABILITY_WINDOW_SECONDS,
                }
        else:
            stable_since = None
        time.sleep(0.2)
    raise TimeoutError(
        f"timed out waiting for the official app to use {managed_cli_path}"
    )


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


def validate_timeout_seconds(timeout_seconds: float) -> None:
    if not math.isfinite(timeout_seconds) or timeout_seconds < MIN_TIMEOUT_SECONDS:
        raise ValueError(
            f"timeout seconds must be finite and at least {MIN_TIMEOUT_SECONDS}"
        )


def desktop_transport_proof(
    gui_pids: list[int], *, log_root: Path | None = None
) -> dict[str, str] | None:
    root = log_root or Path.home() / "Library/Logs/com.openai.codex"
    for pid in gui_pids:
        paths = sorted(
            root.rglob(f"*-{pid}-t0-*.log"),
            key=lambda path: path.stat().st_mtime,
            reverse=True,
        )
        for path in paths:
            log = _bounded_file_tail(path, MAX_DESKTOP_LOG_BYTES)
            if "stdio_transport_spawned" in log:
                raise RuntimeError(
                    "official app log reported a bundled stdio app-server"
                )
            lines = log.splitlines()
            transport_ready = any(
                "Transport start success" in line
                and "hostId=local" in line
                and "transport=websocket" in line
                for line in lines
            )
            initialized = any(
                "initialize_handshake_result" in line
                and "outcome=success" in line
                and "transportKind=websocket" in line
                for line in lines
            )
            if transport_ready and initialized:
                return {
                    "desktopLogPath": str(path.resolve()),
                    "desktopTransport": "websocket",
                }
    return None


def _bounded_file_tail(path: Path, limit: int) -> str:
    with path.open("rb") as handle:
        handle.seek(0, os.SEEK_END)
        handle.seek(max(0, handle.tell() - limit))
        return handle.read(limit).decode(errors="replace")


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


def validate_matching_build_provenance(
    embedded: dict[str, Any], managed: dict[str, Any]
) -> None:
    mismatched = [
        field
        for field in PROVENANCE_FIELDS
        if embedded.get(field) != managed.get(field)
    ]
    if mismatched:
        fields = ", ".join(mismatched)
        raise ValueError(
            f"embedded and managed Codex Lab builds do not match: {fields}"
        )


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
        if is_serving_app_server_command(command) and executable_path(pid) == expected
    ]


def is_serving_app_server_command(command: str) -> bool:
    tokens = command.split()
    return any(
        token == "app-server"
        and (index + 1 == len(tokens) or tokens[index + 1] != "daemon")
        for index, token in enumerate(tokens)
    )


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


def process_has_environment(
    pid: int,
    expected: dict[str, str],
    reader: Callable[[int], str] | None = None,
    *,
    forbidden: set[str] | None = None,
) -> bool:
    environment_reader = reader or read_process_environment
    try:
        process = environment_reader(pid)
    except (OSError, subprocess.SubprocessError):
        return False
    has_expected = all(
        re.search(rf"(?:^|\s){re.escape(name)}={re.escape(value)}(?:\s|$)", process)
        for name, value in expected.items()
    )
    forbidden = forbidden or set()
    has_forbidden = any(
        re.search(rf"(?:^|\s){re.escape(name)}=", process) for name in forbidden
    )
    return has_expected and not has_forbidden


def read_process_environment(pid: int) -> str:
    return subprocess.check_output(
        ["/bin/ps", "eww", "-p", str(pid), "-o", "command="], text=True
    )


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
