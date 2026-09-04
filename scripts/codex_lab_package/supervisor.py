"""Install and manage the pinned macOS Codex Lab app-server supervisor."""

from dataclasses import asdict
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import socket
import stat
import subprocess
import tempfile
import time
from typing import Any

from .engine_contract import REQUIRED_CODE_MODE_HOST_ENTITLEMENTS
from .engine_contract import REQUIRED_ENGINE_ENTITLEMENTS
from .layout import APP_SERVER_LABEL
from .layout import APP_SERVER_LISTEN_HOST
from .layout import APP_SERVER_LISTEN_PORT
from .layout import APP_SERVER_RUNNER_RELATIVE_PATH
from .layout import MAX_PROVENANCE_BYTES
from .live_smoke import process_executable_path
from .live_smoke import read_cli_provenance


DEFAULT_LABEL = APP_SERVER_LABEL
LEGACY_LABEL = "dev.everycode.codex-lab.daemon-supervisor"
DEFAULT_LISTEN_HOST = APP_SERVER_LISTEN_HOST
DEFAULT_LISTEN_PORT = APP_SERVER_LISTEN_PORT
MANAGED_CLI_RELATIVE_PATH = Path("packages/standalone/current/codex")
CODE_MODE_HOST_NAME = "codex-code-mode-host"
ALLOW_JIT_ENTITLEMENT_PLUTIL_KEY_PATH = r"com\.apple\.security\.cs\.allow-jit"
ALLOW_UNSIGNED_EXECUTABLE_MEMORY_PLUTIL_KEY_PATH = (
    r"com\.apple\.security\.cs\.allow-unsigned-executable-memory"
)


@dataclass(frozen=True)
class SupervisorTools:
    codesign: Path = Path("/usr/bin/codesign")
    plutil: Path = Path("/usr/bin/plutil")
    shasum: Path = Path("/usr/bin/shasum")


@dataclass(frozen=True)
class EngineIdentity:
    build_channel: str
    build_profile: str
    release_version: str
    sha256: str
    signing_identifier: str
    source_commit: str
    team_identifier: str
    version: str


@dataclass(frozen=True)
class CodeModeHostIdentity:
    sha256: str
    signing_identifier: str
    team_identifier: str


@dataclass(frozen=True)
class SupervisorPaths:
    lab_home: Path
    launch_agents_dir: Path
    label: str = DEFAULT_LABEL
    listen_host: str = DEFAULT_LISTEN_HOST
    listen_port: int = DEFAULT_LISTEN_PORT

    @property
    def managed_cli(self) -> Path:
        return self.lab_home / MANAGED_CLI_RELATIVE_PATH

    @property
    def code_mode_host(self) -> Path:
        return self.managed_cli.with_name(CODE_MODE_HOST_NAME)

    @property
    def supervisor_dir(self) -> Path:
        return self.lab_home / APP_SERVER_RUNNER_RELATIVE_PATH.parent

    @property
    def runner(self) -> Path:
        return self.lab_home / APP_SERVER_RUNNER_RELATIVE_PATH

    @property
    def plist(self) -> Path:
        return self.launch_agents_dir / f"{self.label}.plist"

    @property
    def stdout_log(self) -> Path:
        return self.supervisor_dir / "app-server.stdout.log"

    @property
    def stderr_log(self) -> Path:
        return self.supervisor_dir / "app-server.stderr.log"

    @property
    def listen_url(self) -> str:
        return f"ws://{self.listen_host}:{self.listen_port}"

    @property
    def websocket_url(self) -> str:
        return f"{self.listen_url}/rpc"


def default_supervisor_paths(
    *,
    lab_home: Path | None = None,
    launch_agents_dir: Path | None = None,
) -> SupervisorPaths:
    home = Path.home()
    return SupervisorPaths(
        lab_home=(lab_home or home / ".codex-lab").expanduser().resolve(),
        launch_agents_dir=(launch_agents_dir or home / "Library/LaunchAgents")
        .expanduser()
        .resolve(),
    )


def inspect_engine(
    managed_cli: Path,
    *,
    codesign_path: Path = Path("/usr/bin/codesign"),
) -> EngineIdentity:
    managed_cli = managed_cli.expanduser().absolute()
    _require_managed_executable(managed_cli, description="engine")

    provenance = read_cli_provenance(managed_cli)
    if provenance["build_profile"] != "release":
        raise ValueError("managed Codex Lab engine must use the release build profile")
    signing_identifier, team_identifier = _signing_identity(
        managed_cli,
        codesign_path=codesign_path,
    )
    code_signing_entitlements = _code_signing_entitlements(
        managed_cli,
        codesign_path=codesign_path,
    )
    if set(code_signing_entitlements) != set(REQUIRED_ENGINE_ENTITLEMENTS):
        raise ValueError(
            "managed Codex Lab engine entitlements do not match the release contract"
        )
    return EngineIdentity(
        build_channel=provenance["build_channel"],
        build_profile=provenance["build_profile"],
        release_version=provenance["release_version"],
        sha256=_sha256_file(managed_cli),
        signing_identifier=signing_identifier,
        source_commit=provenance["source_commit"],
        team_identifier=team_identifier,
        version=provenance["version"],
    )


def inspect_code_mode_host(
    path: Path,
    *,
    codesign_path: Path = Path("/usr/bin/codesign"),
) -> CodeModeHostIdentity:
    path = path.expanduser().absolute()
    _require_managed_executable(path, description="Code Mode host")
    signing_identifier, team_identifier = _signing_identity(
        path,
        codesign_path=codesign_path,
    )
    entitlements = _code_signing_entitlements(path, codesign_path=codesign_path)
    if set(entitlements) != set(REQUIRED_CODE_MODE_HOST_ENTITLEMENTS):
        raise ValueError(
            "managed Codex Lab Code Mode host entitlements do not match the release contract"
        )
    return CodeModeHostIdentity(
        sha256=_sha256_file(path),
        signing_identifier=signing_identifier,
        team_identifier=team_identifier,
    )


def _require_managed_executable(path: Path, *, description: str) -> None:
    try:
        mode = path.lstat().st_mode
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"managed Codex Lab {description} is not executable: {path}"
        ) from exc
    if path.is_symlink() or not stat.S_ISREG(mode) or not os.access(path, os.X_OK):
        raise FileNotFoundError(
            f"managed Codex Lab {description} is not executable: {path}"
        )
    if mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise ValueError(
            f"managed Codex Lab {description} must not be group/world writable"
        )


def _signing_identity(path: Path, *, codesign_path: Path) -> tuple[str, str]:
    subprocess.run(
        [str(codesign_path), "--verify", "--strict", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    signature = subprocess.run(
        [str(codesign_path), "-dvvv", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    signature_output = signature.stdout + signature.stderr
    signing_identifier = _signature_field(signature_output, "Identifier")
    team_identifier = _signature_field(signature_output, "TeamIdentifier")
    if not signing_identifier or not team_identifier or team_identifier == "not set":
        raise ValueError("managed Codex Lab executable lacks a stable signing identity")
    if (
        re.search(r"^CodeDirectory .* flags=.*runtime", signature_output, re.MULTILINE)
        is None
    ):
        raise ValueError("managed Codex Lab executable lacks hardened runtime")
    return signing_identifier, team_identifier


def build_supervisor_runner(
    paths: SupervisorPaths,
    identity: EngineIdentity,
    *,
    code_mode_host_identity: CodeModeHostIdentity | None = None,
    tools: SupervisorTools = SupervisorTools(),
    blocked_retry_seconds: int = 60,
) -> str:
    if blocked_retry_seconds <= 0:
        raise ValueError("supervisor retry interval must be greater than zero")

    def quote(value: object) -> str:
        return shlex.quote(str(value))

    if code_mode_host_identity is None:
        code_mode_host_variables = ""
        verify_code_mode_host = "verify_code_mode_host() { return 0; }"
    else:
        code_mode_host_variables = f"""CODE_MODE_HOST={quote(paths.code_mode_host)}
EXPECTED_CODE_MODE_HOST_SHA256={quote(code_mode_host_identity.sha256)}
EXPECTED_CODE_MODE_HOST_SIGNING_IDENTIFIER={quote(code_mode_host_identity.signing_identifier)}
EXPECTED_CODE_MODE_HOST_TEAM_IDENTIFIER={quote(code_mode_host_identity.team_identifier)}"""
        verify_code_mode_host = """verify_code_mode_host() {
  [ -x "$CODE_MODE_HOST" ] || return 1
  actual_sha256=$("$SHASUM" -a 256 "$CODE_MODE_HOST" | /usr/bin/awk '{ print $1 }')
  [ "$actual_sha256" = "$EXPECTED_CODE_MODE_HOST_SHA256" ] || return 1
  "$CODESIGN" --verify --strict "$CODE_MODE_HOST" >/dev/null 2>&1 || return 1
  signature=$("$CODESIGN" -dvvv "$CODE_MODE_HOST" 2>&1 || true)
  printf '%s\n' "$signature" | /usr/bin/grep -Eq '^CodeDirectory .* flags=.*runtime' || return 1
  signing_identifier=$(printf '%s\n' "$signature" | /usr/bin/awk -F= '$1 == "Identifier" { print substr($0, index($0, "=") + 1); exit }')
  team_identifier=$(printf '%s\n' "$signature" | /usr/bin/awk -F= '$1 == "TeamIdentifier" { print substr($0, index($0, "=") + 1); exit }')
  [ "$signing_identifier" = "$EXPECTED_CODE_MODE_HOST_SIGNING_IDENTIFIER" ] || return 1
  [ "$team_identifier" = "$EXPECTED_CODE_MODE_HOST_TEAM_IDENTIFIER" ] || return 1
  ENTITLEMENTS_FILE=$(/usr/bin/mktemp "${TMPDIR:-/tmp}/codex-lab-host-entitlements.XXXXXX")
  if ! "$CODESIGN" -d --entitlements :- "$CODE_MODE_HOST" >"$ENTITLEMENTS_FILE" 2>/dev/null; then
    discard_entitlements
    return 1
  fi
  if ! "$PLUTIL" -convert xml1 "$ENTITLEMENTS_FILE"; then
    discard_entitlements
    return 1
  fi
  allow_jit=$("$PLUTIL" -extract "$ALLOW_JIT_ENTITLEMENT_KEY_PATH" raw -o - "$ENTITLEMENTS_FILE" 2>/dev/null || true)
  allow_unsigned_executable_memory=$("$PLUTIL" -extract "$ALLOW_UNSIGNED_EXECUTABLE_MEMORY_KEY_PATH" raw -o - "$ENTITLEMENTS_FILE" 2>/dev/null || true)
  entitlement_key_count=$(/usr/bin/grep -o '<key>' "$ENTITLEMENTS_FILE" | /usr/bin/wc -l | /usr/bin/tr -d '[:space:]')
  discard_entitlements
  [ "$allow_jit" = true ] \
    && [ "$allow_unsigned_executable_memory" = true ] \
    && [ "$entitlement_key_count" = 2 ]
}"""

    return f"""#!/bin/sh
set -u
LAB_HOME={quote(paths.lab_home)}
MANAGED_CLI={quote(paths.managed_cli)}
LISTEN_HOST={quote(paths.listen_host)}
LISTEN_PORT={paths.listen_port}
LISTEN_URL={quote(paths.listen_url)}
EXPECTED_SHA256={quote(identity.sha256)}
EXPECTED_SOURCE_COMMIT={quote(identity.source_commit)}
EXPECTED_RELEASE_VERSION={quote(identity.release_version)}
EXPECTED_VERSION={quote(identity.version)}
EXPECTED_SIGNING_IDENTIFIER={quote(identity.signing_identifier)}
EXPECTED_TEAM_IDENTIFIER={quote(identity.team_identifier)}
{code_mode_host_variables}
ALLOW_JIT_ENTITLEMENT_KEY_PATH={quote(ALLOW_JIT_ENTITLEMENT_PLUTIL_KEY_PATH)}
ALLOW_UNSIGNED_EXECUTABLE_MEMORY_KEY_PATH={quote(ALLOW_UNSIGNED_EXECUTABLE_MEMORY_PLUTIL_KEY_PATH)}
CODESIGN={quote(tools.codesign)}
PLUTIL={quote(tools.plutil)}
SHASUM={quote(tools.shasum)}
BLOCKED_RETRY_SECONDS={blocked_retry_seconds}
MAX_PROVENANCE_BYTES={MAX_PROVENANCE_BYTES}
UPDATER_PID_FILE="$LAB_HOME/app-server-daemon/app-server-updater.pid"
DAEMON_PID_FILE="$LAB_HOME/app-server-daemon/app-server.pid"
PROVENANCE_FILE=
ENTITLEMENTS_FILE=
LAST_STATE=

cleanup() {{
  [ -z "$PROVENANCE_FILE" ] || /bin/rm -f "$PROVENANCE_FILE"
  [ -z "$ENTITLEMENTS_FILE" ] || /bin/rm -f "$ENTITLEMENTS_FILE"
}}
discard_provenance() {{
  cleanup
  PROVENANCE_FILE=
}}
discard_entitlements() {{
  [ -z "$ENTITLEMENTS_FILE" ] || /bin/rm -f "$ENTITLEMENTS_FILE"
  ENTITLEMENTS_FILE=
}}
log_state() {{
  [ "$1" = "$LAST_STATE" ] && return
  printf '%s %s\n' "$(/bin/date -u '+%Y-%m-%dT%H:%M:%SZ')" "$1" >&2
  LAST_STATE=$1
}}
json_field() {{
  "$PLUTIL" -extract "$2" raw -o - "$1" 2>/dev/null || true
}}
verify_no_updater() {{
  [ ! -f "$UPDATER_PID_FILE" ] && return 0
  updater_pid=$(json_field "$UPDATER_PID_FILE" pid)
  case "$updater_pid" in ''|*[!0-9]*) return 0 ;; esac
  /bin/kill -0 "$updater_pid" 2>/dev/null || return 0
  updater_command=$(/bin/ps -p "$updater_pid" -o command= 2>/dev/null || true)
  case " $updater_command " in
    *" $MANAGED_CLI app-server daemon pid-update-loop "*) return 1 ;;
    *) return 0 ;;
  esac
}}
verify_no_pid_daemon() {{
  [ ! -f "$DAEMON_PID_FILE" ] && return 0
  daemon_pid=$(json_field "$DAEMON_PID_FILE" pid)
  case "$daemon_pid" in ''|*[!0-9]*) return 0 ;; esac
  /bin/kill -0 "$daemon_pid" 2>/dev/null || return 0
  daemon_command=$(/bin/ps -p "$daemon_pid" -o command= 2>/dev/null || true)
  case " $daemon_command " in
    *" $MANAGED_CLI app-server --remote-control --listen unix:// "*) return 1 ;;
    *) return 0 ;;
  esac
}}
{verify_code_mode_host}
verify_engine() {{
  [ -x "$MANAGED_CLI" ] || return 1
  actual_sha256=$("$SHASUM" -a 256 "$MANAGED_CLI" | /usr/bin/awk '{{ print $1 }}')
  [ "$actual_sha256" = "$EXPECTED_SHA256" ] || return 1
  "$CODESIGN" --verify --strict "$MANAGED_CLI" >/dev/null 2>&1 || return 1
  signature=$("$CODESIGN" -dvvv "$MANAGED_CLI" 2>&1 || true)
  printf '%s\n' "$signature" | /usr/bin/grep -Eq '^CodeDirectory .* flags=.*runtime' || return 1
  signing_identifier=$(printf '%s\n' "$signature" | /usr/bin/awk -F= '$1 == "Identifier" {{ print substr($0, index($0, "=") + 1); exit }}')
  team_identifier=$(printf '%s\n' "$signature" | /usr/bin/awk -F= '$1 == "TeamIdentifier" {{ print substr($0, index($0, "=") + 1); exit }}')
  [ "$signing_identifier" = "$EXPECTED_SIGNING_IDENTIFIER" ] || return 1
  [ "$team_identifier" = "$EXPECTED_TEAM_IDENTIFIER" ] || return 1
  ENTITLEMENTS_FILE=$(/usr/bin/mktemp "${{TMPDIR:-/tmp}}/codex-lab-entitlements.XXXXXX")
  if ! "$CODESIGN" -d --entitlements :- "$MANAGED_CLI" >"$ENTITLEMENTS_FILE" 2>/dev/null; then
    discard_entitlements
    return 1
  fi
  if ! "$PLUTIL" -convert xml1 "$ENTITLEMENTS_FILE"; then
    discard_entitlements
    return 1
  fi
  allow_jit=$("$PLUTIL" -extract "$ALLOW_JIT_ENTITLEMENT_KEY_PATH" raw -o - "$ENTITLEMENTS_FILE" 2>/dev/null || true)
  allow_unsigned_executable_memory=$("$PLUTIL" -extract "$ALLOW_UNSIGNED_EXECUTABLE_MEMORY_KEY_PATH" raw -o - "$ENTITLEMENTS_FILE" 2>/dev/null || true)
  entitlement_key_count=$(/usr/bin/grep -o '<key>' "$ENTITLEMENTS_FILE" | /usr/bin/wc -l | /usr/bin/tr -d '[:space:]')
  discard_entitlements
  [ "$allow_jit" = true ] \
    && [ "$allow_unsigned_executable_memory" = true ] \
    && [ "$entitlement_key_count" = 2 ] \
    || return 1
  PROVENANCE_FILE=$(/usr/bin/mktemp "${{TMPDIR:-/tmp}}/codex-lab-supervisor.XXXXXX")
  if ! "$MANAGED_CLI" debug provenance --json >"$PROVENANCE_FILE"; then
    discard_provenance
    return 1
  fi
  provenance_bytes=$(/usr/bin/wc -c <"$PROVENANCE_FILE" | /usr/bin/tr -d '[:space:]')
  case "$provenance_bytes" in
    ''|*[!0-9]*) discard_provenance; return 1 ;;
  esac
  if [ "$provenance_bytes" -eq 0 ] || [ "$provenance_bytes" -gt "$MAX_PROVENANCE_BYTES" ]; then
    discard_provenance
    return 1
  fi
  schema_version=$(json_field "$PROVENANCE_FILE" schema_version)
  version=$(json_field "$PROVENANCE_FILE" version)
  release_version=$(json_field "$PROVENANCE_FILE" release_version)
  source_commit=$(json_field "$PROVENANCE_FILE" source_commit)
  dirty_state=$(json_field "$PROVENANCE_FILE" dirty_state)
  executable_path=$(json_field "$PROVENANCE_FILE" executable_path)
  discard_provenance
  [ "$schema_version" = 2 ] \
    && [ "$version" = "$EXPECTED_VERSION" ] \
    && [ "$release_version" = "$EXPECTED_RELEASE_VERSION" ] \
    && [ "$source_commit" = "$EXPECTED_SOURCE_COMMIT" ] \
    && [ "$dirty_state" = clean ] \
    && [ "$executable_path" -ef "$MANAGED_CLI" ] \
    && verify_code_mode_host \
    && verify_no_updater \
    && verify_no_pid_daemon
}}

command=${{1:-run}}
if [ "$command" = check ]; then
  verify_engine
  exit $?
fi
[ "$command" = run ] || {{ echo "usage: $0 [run|check]" >&2; exit 64; }}
trap cleanup EXIT
trap 'exit 0' HUP INT TERM
while :; do
  if ! verify_engine; then
    log_state "state=blocked reason=engine-validation"
    /bin/sleep "$BLOCKED_RETRY_SECONDS"
    continue
  fi
  if /usr/bin/nc -z "$LISTEN_HOST" "$LISTEN_PORT" >/dev/null 2>&1; then
    log_state "state=blocked reason=listen-port-occupied"
    /bin/sleep "$BLOCKED_RETRY_SECONDS"
    continue
  fi
  break
done
log_state "state=starting url=$LISTEN_URL"
exec /usr/bin/env CODEX_HOME="$LAB_HOME" CODEX_LAB_HOME="$LAB_HOME" \
  "$MANAGED_CLI" app-server --remote-control --listen "$LISTEN_URL"
"""


def build_launch_agent_plist(paths: SupervisorPaths) -> bytes:
    return plistlib.dumps(
        {
            "EnvironmentVariables": {
                "CODEX_HOME": str(paths.lab_home),
                "CODEX_LAB_HOME": str(paths.lab_home),
            },
            "ExitTimeOut": 10,
            "KeepAlive": True,
            "Label": paths.label,
            "ProcessType": "Background",
            "ProgramArguments": [str(paths.runner), "run"],
            "RunAtLoad": True,
            "StandardErrorPath": str(paths.stderr_log),
            "StandardOutPath": str(paths.stdout_log),
            "ThrottleInterval": 1,
        },
        sort_keys=True,
    )


def install_supervisor(
    paths: SupervisorPaths,
    *,
    expected_sha256: str,
    expected_source_commit: str,
    expected_release_version: str,
    expected_version: str,
    expected_code_mode_host_sha256: str | None = None,
    expected_code_mode_host_signing_identifier: str | None = None,
    expected_code_mode_host_team_identifier: str | None = None,
    launchctl_path: Path = Path("/bin/launchctl"),
    tools: SupervisorTools = SupervisorTools(),
    uid: int | None = None,
    health_timeout_seconds: float = 75.0,
) -> dict[str, Any]:
    _stop_updater(paths)
    identity = inspect_engine(paths.managed_cli, codesign_path=tools.codesign)
    _require_expected_identity(
        identity,
        expected_sha256=expected_sha256,
        expected_source_commit=expected_source_commit,
        expected_release_version=expected_release_version,
        expected_version=expected_version,
    )
    code_mode_host_identity = None
    expected_host_values = (
        expected_code_mode_host_sha256,
        expected_code_mode_host_signing_identifier,
        expected_code_mode_host_team_identifier,
    )
    if any(value is not None for value in expected_host_values):
        if any(value is None for value in expected_host_values):
            raise ValueError("expected Code Mode host identity must be complete")
        code_mode_host_identity = inspect_code_mode_host(
            paths.code_mode_host,
            codesign_path=tools.codesign,
        )
        actual_host_values = (
            code_mode_host_identity.sha256,
            code_mode_host_identity.signing_identifier,
            code_mode_host_identity.team_identifier,
        )
        if actual_host_values != expected_host_values:
            raise ValueError(
                "managed Code Mode host does not match the expected candidate"
            )
    _stop_pid_daemon(paths)
    service = _service_name(paths.label, uid)
    domain = service.rsplit("/", maxsplit=1)[0]
    current_pid = _launchctl_pid(launchctl_path, service)
    if _port_is_listening(paths.listen_host, paths.listen_port) and not (
        current_pid and _pid_listens(current_pid, paths.listen_port)
    ):
        raise RuntimeError(f"listen port {paths.listen_port} is already occupied")
    previous_runner = _snapshot(paths.runner)
    previous_plist = _snapshot(paths.plist)
    was_loaded = current_pid is not None
    old_service_stopped = False
    new_service_bootstrapped = False
    try:
        _write_atomic(
            paths.runner,
            build_supervisor_runner(
                paths,
                identity,
                code_mode_host_identity=code_mode_host_identity,
                tools=tools,
            ).encode(),
            0o755,
        )
        _write_atomic(paths.plist, build_launch_agent_plist(paths), 0o644)
        subprocess.run([str(paths.runner), "check"], check=True)
        if was_loaded:
            _launchctl(launchctl_path, "bootout", service)
            old_service_stopped = True
        _launchctl(launchctl_path, "bootstrap", domain, str(paths.plist))
        new_service_bootstrapped = True
        _launchctl(launchctl_path, "kickstart", "-k", service)
        _wait_for_health(paths, launchctl_path, service, health_timeout_seconds)
    except Exception as install_error:
        try:
            if new_service_bootstrapped:
                _launchctl(launchctl_path, "bootout", service, check=False)
            _restore(paths.runner, previous_runner)
            _restore(paths.plist, previous_plist)
            if old_service_stopped and previous_plist is not None:
                _launchctl(launchctl_path, "bootstrap", domain, str(paths.plist))
                _launchctl(launchctl_path, "kickstart", "-k", service)
        except Exception as rollback_error:
            raise RuntimeError(
                f"supervisor rollback failed: {rollback_error}"
            ) from install_error
        raise
    _remove_legacy_supervisor(paths, launchctl_path, uid)
    return {
        "engine": asdict(identity),
        "codeModeHost": asdict(code_mode_host_identity)
        if code_mode_host_identity is not None
        else None,
        "label": paths.label,
        "plistPath": str(paths.plist),
        "runnerPath": str(paths.runner),
        "schemaVersion": 1,
        "service": service,
        "status": "installed",
        "websocketUrl": paths.websocket_url,
    }


def supervisor_status(
    paths: SupervisorPaths,
    *,
    launchctl_path: Path = Path("/bin/launchctl"),
    uid: int | None = None,
) -> dict[str, Any]:
    service = _service_name(paths.label, uid)
    pid = _launchctl_pid(launchctl_path, service)
    pin_valid = (
        paths.runner.is_file()
        and subprocess.run([str(paths.runner), "check"], capture_output=True).returncode
        == 0
    )
    process_matches = pid is not None and _pid_matches(paths, pid)
    listening = pid is not None and _pid_listens(pid, paths.listen_port)
    return {
        "healthy": bool(pin_valid and process_matches and listening),
        "installed": paths.runner.is_file() and paths.plist.is_file(),
        "label": paths.label,
        "listening": listening,
        "loaded": pid is not None,
        "pid": pid,
        "processMatches": process_matches,
        "schemaVersion": 1,
        "service": service,
        "updaterRunning": _updater_pid(paths) is not None,
        "websocketUrl": paths.websocket_url,
    }


def uninstall_supervisor(
    paths: SupervisorPaths,
    *,
    launchctl_path: Path = Path("/bin/launchctl"),
    uid: int | None = None,
) -> dict[str, Any]:
    service = _service_name(paths.label, uid)
    if _launchctl_pid(launchctl_path, service) is not None:
        _launchctl(launchctl_path, "bootout", service)
    _stop_updater(paths)
    _stop_pid_daemon(paths)
    _remove_legacy_supervisor(paths, launchctl_path, uid)
    paths.plist.unlink(missing_ok=True)
    shutil.rmtree(paths.supervisor_dir, ignore_errors=True)
    return {
        "label": paths.label,
        "schemaVersion": 1,
        "service": service,
        "status": "uninstalled",
    }


def _signature_field(output: str, name: str) -> str:
    prefix = f"{name}="
    return next(
        (
            line.removeprefix(prefix).strip()
            for line in output.splitlines()
            if line.startswith(prefix)
        ),
        "",
    )


def _code_signing_entitlements(
    path: Path,
    *,
    codesign_path: Path,
) -> tuple[str, ...]:
    completed = subprocess.run(
        [str(codesign_path), "-d", "--entitlements", ":-", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    if not completed.stdout.strip():
        return ()
    try:
        entitlements = plistlib.loads(completed.stdout.encode())
    except plistlib.InvalidFileException as exc:
        raise ValueError("managed Codex Lab engine has invalid entitlements") from exc
    if not isinstance(entitlements, dict):
        raise ValueError("managed Codex Lab engine has invalid entitlements")
    return tuple(sorted(key for key, value in entitlements.items() if value is True))


def _require_expected_identity(
    identity: EngineIdentity,
    *,
    expected_sha256: str,
    expected_source_commit: str,
    expected_release_version: str,
    expected_version: str,
) -> None:
    actual = (
        identity.sha256,
        identity.source_commit,
        identity.release_version,
        identity.version,
    )
    expected = (
        expected_sha256,
        expected_source_commit,
        expected_release_version,
        expected_version,
    )
    if actual != expected:
        raise ValueError(
            "managed Codex Lab engine does not match the expected candidate"
        )


def _updater_pid(paths: SupervisorPaths) -> int | None:
    pid_file = paths.lab_home / "app-server-daemon/app-server-updater.pid"
    try:
        pid = json.loads(pid_file.read_text(encoding="utf-8"))["pid"]
    except (FileNotFoundError, KeyError, TypeError, ValueError, json.JSONDecodeError):
        return None
    if not isinstance(pid, int) or pid <= 1:
        return None
    try:
        executable = process_executable_path(pid)
        command = subprocess.check_output(
            ["/bin/ps", "-p", str(pid), "-o", "command="], text=True
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if executable != paths.managed_cli.resolve():
        return None
    return pid if "app-server daemon pid-update-loop" in command else None


def _stop_updater(paths: SupervisorPaths) -> None:
    pid_file = paths.lab_home / "app-server-daemon/app-server-updater.pid"
    pid = _updater_pid(paths)
    if pid is None:
        pid_file.unlink(missing_ok=True)
        return
    os.kill(pid, 15)
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if _updater_pid(paths) is None:
            pid_file.unlink(missing_ok=True)
            return
        time.sleep(0.1)
    os.kill(pid, 9)
    time.sleep(0.1)
    if _updater_pid(paths) is None:
        pid_file.unlink(missing_ok=True)
        return
    raise TimeoutError(f"timed out stopping Codex Lab updater process {pid}")


def _stop_pid_daemon(paths: SupervisorPaths) -> None:
    completed = subprocess.run(
        [str(paths.managed_cli), "app-server", "daemon", "stop"],
        capture_output=True,
        env={
            **os.environ,
            "CODEX_HOME": str(paths.lab_home),
            "CODEX_LAB_HOME": str(paths.lab_home),
        },
        text=True,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise RuntimeError(f"failed to stop legacy PID daemon: {detail}")


def _service_name(label: str, uid: int | None) -> str:
    return f"gui/{os.getuid() if uid is None else uid}/{label}"


def _launchctl_pid(launchctl_path: Path, service: str) -> int | None:
    completed = subprocess.run(
        [str(launchctl_path), "print", service], capture_output=True, text=True
    )
    if completed.returncode != 0:
        return None
    match = re.search(r"^\s*pid = ([0-9]+)$", completed.stdout, re.MULTILINE)
    return int(match.group(1)) if match else None


def _launchctl(
    launchctl_path: Path, *args: str, check: bool = True
) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        [str(launchctl_path), *args], capture_output=True, text=True
    )
    if check and completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise RuntimeError(f"launchctl {' '.join(args)} failed: {detail}")
    if args[:1] == ("bootout",) and completed.returncode == 0:
        deadline = time.monotonic() + 10
        while _launchctl_pid(launchctl_path, args[1]) is not None:
            if time.monotonic() >= deadline:
                raise TimeoutError(f"timed out unloading {args[1]}")
            time.sleep(0.1)
    return completed


def _wait_for_health(
    paths: SupervisorPaths,
    launchctl_path: Path,
    service: str,
    timeout_seconds: float,
) -> None:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        pid = _launchctl_pid(launchctl_path, service)
        if pid and _pid_matches(paths, pid) and _pid_listens(pid, paths.listen_port):
            return
        time.sleep(0.25)
    raise TimeoutError("Codex Lab app-server supervisor did not become healthy")


def _pid_matches(paths: SupervisorPaths, pid: int) -> bool:
    try:
        command = subprocess.check_output(
            ["/bin/ps", "-p", str(pid), "-o", "command="], text=True
        )
    except subprocess.SubprocessError:
        return False
    return (
        process_executable_path(pid) == paths.managed_cli.resolve()
        and " app-server " in f" {command.strip()} "
        and " --remote-control " in f" {command.strip()} "
        and f" --listen {paths.listen_url} " in f" {command.strip()} "
    )


def _pid_listens(pid: int, port: int) -> bool:
    return (
        subprocess.run(
            [
                "/usr/sbin/lsof",
                "-n",
                "-P",
                "-a",
                "-p",
                str(pid),
                f"-iTCP:{port}",
                "-sTCP:LISTEN",
            ],
            capture_output=True,
        ).returncode
        == 0
    )


def _port_is_listening(host: str, port: int) -> bool:
    try:
        with socket.create_connection((host, port), timeout=0.2):
            return True
    except OSError:
        return False


def _remove_legacy_supervisor(
    paths: SupervisorPaths, launchctl_path: Path, uid: int | None
) -> None:
    service = _service_name(LEGACY_LABEL, uid)
    if _launchctl_pid(launchctl_path, service) is not None:
        _launchctl(launchctl_path, "bootout", service)
    (paths.launch_agents_dir / f"{LEGACY_LABEL}.plist").unlink(missing_ok=True)
    (paths.lab_home / "supervisor/codex-lab-daemon-supervisor").unlink(missing_ok=True)


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _write_atomic(path: Path, contents: bytes, mode: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temp_path = Path(temp_name)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(contents)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temp_path, mode)
        os.replace(temp_path, path)
    finally:
        temp_path.unlink(missing_ok=True)


def _snapshot(path: Path) -> tuple[bytes, int] | None:
    try:
        return path.read_bytes(), stat.S_IMODE(path.stat().st_mode)
    except FileNotFoundError:
        return None


def _restore(path: Path, snapshot: tuple[bytes, int] | None) -> None:
    if snapshot is None:
        path.unlink(missing_ok=True)
    else:
        _write_atomic(path, snapshot[0], snapshot[1])
