"""Install and manage the pinned macOS Codex Lab daemon supervisor."""

from dataclasses import asdict
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shlex
import shutil
import stat
import subprocess
import tempfile
import time
from typing import Any

from .layout import MAX_PROVENANCE_BYTES
from .live_smoke import process_executable_path
from .live_smoke import read_cli_provenance


DEFAULT_LABEL = "dev.everycode.codex-lab.daemon-supervisor"
MANAGED_CLI_RELATIVE_PATH = Path("packages/standalone/current/codex")
SUPERVISOR_RELATIVE_PATH = Path("supervisor/codex-lab-daemon-supervisor")


@dataclass(frozen=True)
class SupervisorTools:
    codesign: Path = Path("/usr/bin/codesign")
    plutil: Path = Path("/usr/bin/plutil")
    shasum: Path = Path("/usr/bin/shasum")


@dataclass(frozen=True)
class EngineIdentity:
    build_channel: str
    build_profile: str
    sha256: str
    signing_identifier: str
    source_commit: str
    team_identifier: str
    version: str


@dataclass(frozen=True)
class SupervisorPaths:
    lab_home: Path
    launch_agents_dir: Path
    label: str = DEFAULT_LABEL

    @property
    def managed_cli(self) -> Path:
        return self.lab_home / MANAGED_CLI_RELATIVE_PATH

    @property
    def runner(self) -> Path:
        return self.lab_home / SUPERVISOR_RELATIVE_PATH

    @property
    def plist(self) -> Path:
        return self.launch_agents_dir / f"{self.label}.plist"

    @property
    def stdout_log(self) -> Path:
        return self.lab_home / "supervisor/supervisor.stdout.log"

    @property
    def stderr_log(self) -> Path:
        return self.lab_home / "supervisor/supervisor.stderr.log"


def default_supervisor_paths(
    *,
    lab_home: Path | None = None,
    launch_agents_dir: Path | None = None,
    label: str = DEFAULT_LABEL,
) -> SupervisorPaths:
    home = Path.home()
    return SupervisorPaths(
        lab_home=(lab_home or home / ".codex-lab").expanduser().resolve(),
        launch_agents_dir=(launch_agents_dir or home / "Library/LaunchAgents")
        .expanduser()
        .resolve(),
        label=label,
    )


def inspect_engine(
    managed_cli: Path,
    *,
    codesign_path: Path = Path("/usr/bin/codesign"),
) -> EngineIdentity:
    managed_cli = managed_cli.expanduser().absolute()
    try:
        mode = managed_cli.lstat().st_mode
    except FileNotFoundError as exc:
        raise FileNotFoundError(
            f"managed Codex Lab engine is not executable: {managed_cli}"
        ) from exc
    if (
        managed_cli.is_symlink()
        or not stat.S_ISREG(mode)
        or not os.access(managed_cli, os.X_OK)
    ):
        raise FileNotFoundError(
            f"managed Codex Lab engine is not executable: {managed_cli}"
        )
    if mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise ValueError("managed Codex Lab engine must not be group/world writable")

    provenance = read_cli_provenance(managed_cli)
    if provenance["build_profile"] != "release":
        raise ValueError("managed Codex Lab engine must use the release build profile")

    subprocess.run(
        [str(codesign_path), "--verify", "--strict", str(managed_cli)],
        check=True,
        capture_output=True,
        text=True,
    )
    signature = subprocess.run(
        [str(codesign_path), "-dvvv", str(managed_cli)],
        check=True,
        capture_output=True,
        text=True,
    )
    signature_output = signature.stdout + signature.stderr
    signing_identifier = _signature_field(signature_output, "Identifier")
    team_identifier = _signature_field(signature_output, "TeamIdentifier")
    if not signing_identifier or not team_identifier or team_identifier == "not set":
        raise ValueError("managed Codex Lab engine lacks a stable signing identity")

    return EngineIdentity(
        build_channel=provenance["build_channel"],
        build_profile=provenance["build_profile"],
        sha256=_sha256_file(managed_cli),
        signing_identifier=signing_identifier,
        source_commit=provenance["source_commit"],
        team_identifier=team_identifier,
        version=provenance["version"],
    )


def build_supervisor_runner(
    paths: SupervisorPaths,
    identity: EngineIdentity,
    *,
    tools: SupervisorTools = SupervisorTools(),
    poll_seconds: int = 2,
    blocked_retry_seconds: int = 60,
) -> str:
    if poll_seconds <= 0 or blocked_retry_seconds <= 0:
        raise ValueError("supervisor retry intervals must be greater than zero")

    quote = lambda value: shlex.quote(str(value))
    return f"""#!/bin/sh
set -u

LAB_HOME={quote(paths.lab_home)}
MANAGED_CLI={quote(paths.managed_cli)}
EXPECTED_SHA256={quote(identity.sha256)}
EXPECTED_SOURCE_COMMIT={quote(identity.source_commit)}
EXPECTED_VERSION={quote(identity.version)}
EXPECTED_BUILD_PROFILE={quote(identity.build_profile)}
EXPECTED_BUILD_CHANNEL={quote(identity.build_channel)}
EXPECTED_SIGNING_IDENTIFIER={quote(identity.signing_identifier)}
EXPECTED_TEAM_IDENTIFIER={quote(identity.team_identifier)}
EXPECTED_SOCKET="$LAB_HOME/app-server-control/app-server-control.sock"
CODESIGN={quote(tools.codesign)}
PLUTIL={quote(tools.plutil)}
SHASUM={quote(tools.shasum)}
POLL_SECONDS={poll_seconds}
BLOCKED_RETRY_SECONDS={blocked_retry_seconds}
RECOVERY_ATTEMPTS=120
MAX_PROVENANCE_BYTES={MAX_PROVENANCE_BYTES}
UPDATER_PID_FILE="$LAB_HOME/app-server-daemon/app-server-updater.pid"
PROVENANCE_FILE=
DAEMON_FILE=
LAST_STATE=

cleanup() {{
  [ -z "$PROVENANCE_FILE" ] || /bin/rm -f "$PROVENANCE_FILE"
  [ -z "$DAEMON_FILE" ] || /bin/rm -f "$DAEMON_FILE"
}}

log_state() {{
  state=$1
  if [ "$state" != "$LAST_STATE" ]; then
    printf '%s %s\n' "$(/bin/date -u '+%Y-%m-%dT%H:%M:%SZ')" "$state" >&2
    LAST_STATE=$state
  fi
}}

json_field() {{
  "$PLUTIL" -extract "$2" raw -o - "$1" 2>/dev/null || true
}}

verify_no_updater() {{
  if [ ! -f "$UPDATER_PID_FILE" ]; then
    return 0
  fi
  updater_pid=$(json_field "$UPDATER_PID_FILE" pid)
  case "$updater_pid" in
    ''|*[!0-9]*) return 1 ;;
  esac
  if ! /bin/kill -0 "$updater_pid" 2>/dev/null; then
    return 0
  fi
  updater_command=$(/bin/ps -p "$updater_pid" -o command= 2>/dev/null || true)
  case " $updater_command " in
    *" $MANAGED_CLI app-server daemon pid-update-loop "*) return 1 ;;
    *) return 0 ;;
  esac
}}

verify_engine() {{
  if [ ! -x "$MANAGED_CLI" ]; then
    return 1
  fi
  actual_sha256=$("$SHASUM" -a 256 "$MANAGED_CLI" | /usr/bin/awk '{{ print $1 }}')
  if [ "$actual_sha256" != "$EXPECTED_SHA256" ]; then
    return 1
  fi
  if ! "$CODESIGN" --verify --strict "$MANAGED_CLI" >/dev/null 2>&1; then
    return 1
  fi
  signature=$("$CODESIGN" -dvvv "$MANAGED_CLI" 2>&1 || true)
  signing_identifier=$(printf '%s\n' "$signature" | /usr/bin/awk -F= '$1 == "Identifier" {{ print substr($0, index($0, "=") + 1); exit }}')
  team_identifier=$(printf '%s\n' "$signature" | /usr/bin/awk -F= '$1 == "TeamIdentifier" {{ print substr($0, index($0, "=") + 1); exit }}')
  if [ "$signing_identifier" != "$EXPECTED_SIGNING_IDENTIFIER" ] \
    || [ "$team_identifier" != "$EXPECTED_TEAM_IDENTIFIER" ]; then
    return 1
  fi

  PROVENANCE_FILE=$(/usr/bin/mktemp "${{TMPDIR:-/tmp}}/codex-lab-supervisor-provenance.XXXXXX")
  if ! "$MANAGED_CLI" debug provenance --json >"$PROVENANCE_FILE"; then
    /bin/rm -f "$PROVENANCE_FILE"
    PROVENANCE_FILE=
    return 1
  fi
  provenance_bytes=$(/usr/bin/wc -c <"$PROVENANCE_FILE" | /usr/bin/tr -d '[:space:]')
  case "$provenance_bytes" in
    ''|*[!0-9]*)
      /bin/rm -f "$PROVENANCE_FILE"
      PROVENANCE_FILE=
      return 1
      ;;
  esac
  if [ "$provenance_bytes" -eq 0 ] || [ "$provenance_bytes" -gt "$MAX_PROVENANCE_BYTES" ]; then
    /bin/rm -f "$PROVENANCE_FILE"
    PROVENANCE_FILE=
    return 1
  fi

  schema_version=$(json_field "$PROVENANCE_FILE" schema_version)
  version=$(json_field "$PROVENANCE_FILE" version)
  source_commit=$(json_field "$PROVENANCE_FILE" source_commit)
  dirty_state=$(json_field "$PROVENANCE_FILE" dirty_state)
  build_profile=$(json_field "$PROVENANCE_FILE" build_profile)
  build_channel=$(json_field "$PROVENANCE_FILE" build_channel)
  executable_path=$(json_field "$PROVENANCE_FILE" executable_path)
  /bin/rm -f "$PROVENANCE_FILE"
  PROVENANCE_FILE=

  [ "$schema_version" = 1 ] \
    && [ "$version" = "$EXPECTED_VERSION" ] \
    && [ "$source_commit" = "$EXPECTED_SOURCE_COMMIT" ] \
    && [ "$dirty_state" = clean ] \
    && [ "$build_profile" = "$EXPECTED_BUILD_PROFILE" ] \
    && [ "$build_channel" = "$EXPECTED_BUILD_CHANNEL" ] \
    && [ "$executable_path" -ef "$MANAGED_CLI" ] \
    && verify_no_updater
}}

verify_daemon() {{
  DAEMON_FILE=$(/usr/bin/mktemp "${{TMPDIR:-/tmp}}/codex-lab-supervisor-daemon.XXXXXX")
  if ! CODEX_HOME="$LAB_HOME" CODEX_LAB_HOME="$LAB_HOME" \
    "$MANAGED_CLI" app-server daemon version >"$DAEMON_FILE" 2>/dev/null; then
    /bin/rm -f "$DAEMON_FILE"
    DAEMON_FILE=
    return 1
  fi
  daemon_status=$(json_field "$DAEMON_FILE" status)
  daemon_backend=$(json_field "$DAEMON_FILE" backend)
  daemon_managed_path=$(json_field "$DAEMON_FILE" managedCodexPath)
  daemon_socket=$(json_field "$DAEMON_FILE" socketPath)
  daemon_version=$(json_field "$DAEMON_FILE" appServerVersion)
  /bin/rm -f "$DAEMON_FILE"
  DAEMON_FILE=

  [ "$daemon_status" = running ] \
    && [ "$daemon_backend" = pid ] \
    && [ "$daemon_managed_path" -ef "$MANAGED_CLI" ] \
    && [ "$daemon_socket" = "$EXPECTED_SOCKET" ] \
    && [ "$daemon_version" = "$EXPECTED_VERSION" ]
}}

recover_daemon() {{
  CODEX_HOME="$LAB_HOME" CODEX_LAB_HOME="$LAB_HOME" \
    "$MANAGED_CLI" app-server daemon start || true
  attempt=0
  while [ "$attempt" -lt "$RECOVERY_ATTEMPTS" ]; do
    verify_daemon && return 0
    attempt=$((attempt + 1))
    /bin/sleep 0.5
  done
  return 1
}}

command=${{1:-run}}
case "$command" in
  check)
    verify_engine
    exit $?
    ;;
  status)
    verify_engine && verify_daemon
    exit $?
    ;;
  run) ;;
  *)
    echo "usage: $0 [run|check|status]" >&2
    exit 64
    ;;
esac

trap cleanup EXIT
trap 'exit 0' HUP INT TERM
while :; do
  if ! verify_engine; then
    log_state "state=blocked reason=engine-validation"
    /bin/sleep "$BLOCKED_RETRY_SECONDS"
    continue
  fi
  if verify_daemon; then
    log_state "state=running version=$EXPECTED_VERSION"
    /bin/sleep "$POLL_SECONDS"
    continue
  fi

  log_state "state=recovering reason=daemon-unavailable"
  if ! recover_daemon; then
    log_state "state=blocked reason=daemon-recovery-timeout"
    /bin/sleep "$BLOCKED_RETRY_SECONDS"
    continue
  fi
  log_state "state=running version=$EXPECTED_VERSION"
done
"""


def build_launch_agent_plist(paths: SupervisorPaths) -> bytes:
    return plistlib.dumps(
        {
            "EnvironmentVariables": {
                "CODEX_HOME": str(paths.lab_home),
                "CODEX_LAB_HOME": str(paths.lab_home),
            },
            "KeepAlive": True,
            "Label": paths.label,
            "ProcessType": "Background",
            "ProgramArguments": [str(paths.runner), "run"],
            "RunAtLoad": True,
            "StandardErrorPath": str(paths.stderr_log),
            "StandardOutPath": str(paths.stdout_log),
            "ThrottleInterval": 10,
        },
        sort_keys=True,
    )


def install_supervisor(
    paths: SupervisorPaths,
    *,
    expected_sha256: str,
    expected_source_commit: str,
    expected_version: str,
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
        expected_version=expected_version,
    )
    runner = build_supervisor_runner(paths, identity, tools=tools)
    plist = build_launch_agent_plist(paths)
    service = _service_name(paths.label, uid)
    domain = service.rsplit("/", maxsplit=1)[0]
    was_loaded = _launchctl_loaded(launchctl_path, service)
    previous_runner = _snapshot(paths.runner)
    previous_plist = _snapshot(paths.plist)

    try:
        _write_atomic(paths.runner, runner.encode(), 0o755)
        _write_atomic(paths.plist, plist, 0o644)
        subprocess.run([str(paths.runner), "check"], check=True)
        if was_loaded:
            _launchctl(launchctl_path, "bootout", service)
        _launchctl(launchctl_path, "bootstrap", domain, str(paths.plist))
        _launchctl(launchctl_path, "kickstart", "-k", service)
        _wait_for_health(paths.runner, health_timeout_seconds)
    except Exception:
        _launchctl(launchctl_path, "bootout", service, check=False)
        _restore(paths.runner, previous_runner)
        _restore(paths.plist, previous_plist)
        if was_loaded and previous_plist is not None:
            _launchctl(launchctl_path, "bootstrap", domain, str(paths.plist))
            _launchctl(launchctl_path, "kickstart", "-k", service)
        raise

    return {
        "engine": asdict(identity),
        "label": paths.label,
        "plistPath": str(paths.plist),
        "runnerPath": str(paths.runner),
        "schemaVersion": 1,
        "service": service,
        "status": "installed",
    }


def supervisor_status(
    paths: SupervisorPaths,
    *,
    launchctl_path: Path = Path("/bin/launchctl"),
    uid: int | None = None,
) -> dict[str, Any]:
    service = _service_name(paths.label, uid)
    loaded = _launchctl_loaded(launchctl_path, service)
    healthy = False
    if paths.runner.is_file():
        healthy = (
            subprocess.run(
                [str(paths.runner), "status"], capture_output=True
            ).returncode
            == 0
        )
    daemon = None
    if paths.managed_cli.is_file():
        completed = subprocess.run(
            [str(paths.managed_cli), "app-server", "daemon", "version"],
            capture_output=True,
            env={
                **os.environ,
                "CODEX_HOME": str(paths.lab_home),
                "CODEX_LAB_HOME": str(paths.lab_home),
            },
            text=True,
        )
        if completed.returncode == 0:
            try:
                daemon = json.loads(completed.stdout)
            except json.JSONDecodeError:
                daemon = {"error": "invalid daemon JSON"}
    return {
        "daemon": daemon,
        "healthy": healthy,
        "installed": paths.runner.is_file() and paths.plist.is_file(),
        "label": paths.label,
        "loaded": loaded,
        "schemaVersion": 1,
        "service": service,
        "updaterRunning": _updater_pid(paths) is not None,
    }


def uninstall_supervisor(
    paths: SupervisorPaths,
    *,
    launchctl_path: Path = Path("/bin/launchctl"),
    uid: int | None = None,
) -> dict[str, Any]:
    service = _service_name(paths.label, uid)
    if _launchctl_loaded(launchctl_path, service):
        _launchctl(launchctl_path, "bootout", service)
    _stop_updater(paths)

    daemon_stopped = False
    if paths.managed_cli.is_file():
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
            raise RuntimeError(
                completed.stderr.strip() or "failed to stop managed daemon"
            )
        daemon_stopped = True

    paths.plist.unlink(missing_ok=True)
    shutil.rmtree(paths.runner.parent, ignore_errors=True)
    return {
        "daemonStopped": daemon_stopped,
        "label": paths.label,
        "schemaVersion": 1,
        "service": service,
        "status": "uninstalled",
    }


def _signature_field(output: str, name: str) -> str:
    prefix = f"{name}="
    for line in output.splitlines():
        if line.startswith(prefix):
            return line.removeprefix(prefix).strip()
    return ""


def _require_expected_identity(
    identity: EngineIdentity,
    *,
    expected_sha256: str,
    expected_source_commit: str,
    expected_version: str,
) -> None:
    expected = {
        "sha256": expected_sha256,
        "source_commit": expected_source_commit,
        "version": expected_version,
    }
    actual = {
        "sha256": identity.sha256,
        "source_commit": identity.source_commit,
        "version": identity.version,
    }
    mismatched = [field for field in expected if expected[field] != actual[field]]
    if mismatched:
        raise ValueError(
            "managed Codex Lab engine does not match the expected candidate: "
            + ", ".join(mismatched)
        )


def _updater_pid(paths: SupervisorPaths) -> int | None:
    pid_file = paths.lab_home / "app-server-daemon/app-server-updater.pid"
    try:
        record = json.loads(pid_file.read_text(encoding="utf-8"))
        pid = record["pid"]
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
    if "app-server daemon pid-update-loop" not in command:
        return None
    return pid


def _stop_updater(paths: SupervisorPaths) -> None:
    pid = _updater_pid(paths)
    if pid is None:
        return
    os.kill(pid, 15)
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if _updater_pid(paths) is None:
            return
        time.sleep(0.1)
    raise TimeoutError(f"timed out stopping Codex Lab updater process {pid}")


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _service_name(label: str, uid: int | None) -> str:
    return f"gui/{os.getuid() if uid is None else uid}/{label}"


def _launchctl_loaded(launchctl_path: Path, service: str) -> bool:
    return (
        subprocess.run(
            [str(launchctl_path), "print", service], capture_output=True
        ).returncode
        == 0
    )


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
        service = args[1]
        deadline = time.monotonic() + 10
        while _launchctl_loaded(launchctl_path, service):
            if time.monotonic() >= deadline:
                raise TimeoutError(f"timed out unloading {service}")
            time.sleep(0.1)
    return completed


def _wait_for_health(runner: Path, timeout_seconds: float) -> None:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        if subprocess.run([str(runner), "status"], capture_output=True).returncode == 0:
            return
        time.sleep(0.25)
    raise TimeoutError("Codex Lab supervisor did not produce a healthy daemon")


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
        return
    contents, mode = snapshot
    _write_atomic(path, contents, mode)
