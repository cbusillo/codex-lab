"""Build the macOS Codex Lab launcher app bundle layout."""

from dataclasses import dataclass
import hashlib
from pathlib import Path
import plistlib
import shutil
import stat

from .icon import ICON_FILE_NAME
from .icon import write_codex_lab_icon


DEFAULT_BUNDLE_IDENTIFIER = "dev.everycode.codex-lab"
APP_SERVER_LABEL = "dev.everycode.codex-lab.app-server.v1"
APP_SERVER_LISTEN_HOST = "127.0.0.1"
APP_SERVER_LISTEN_PORT = 4766
APP_SERVER_LISTEN_URL = f"ws://{APP_SERVER_LISTEN_HOST}:{APP_SERVER_LISTEN_PORT}"
APP_SERVER_WEBSOCKET_URL = f"{APP_SERVER_LISTEN_URL}/rpc"
APP_SERVER_RUNNER_RELATIVE_PATH = Path("supervisor/v1/codex-lab-app-server")
MAX_PROVENANCE_BYTES = 4096
OFFICIAL_APP_BUNDLE_IDENTIFIER = "com.openai.codex"
OFFICIAL_APP_TEAM_IDENTIFIER = "2DC432GLL2"
OFFICIAL_APP_CANDIDATE_PATHS = (
    "/Applications/ChatGPT.app",
    "~/Applications/ChatGPT.app",
    "/Applications/Codex.app",
    "~/Applications/Codex.app",
)
DEFAULT_CODEX_APP_PATH = Path(OFFICIAL_APP_CANDIDATE_PATHS[0])
DEFAULT_DISPLAY_NAME = "Codex Lab"
EMBEDDED_CLI_NAME = "codex-lab"
LAUNCHER_NAME = "Codex Lab Launcher"
SHIM_NAME = "codex-lab"


@dataclass(frozen=True)
class CodexLabAppOptions:
    app_dir: Path
    codex_bin: Path
    codex_app_path: Path = DEFAULT_CODEX_APP_PATH
    shim_dir: Path | None = None
    bundle_identifier: str = DEFAULT_BUNDLE_IDENTIFIER
    display_name: str = DEFAULT_DISPLAY_NAME
    short_version: str = "0.0.0"
    release_version: str | None = None
    bundle_version: str = "1"
    embedded_cli_version: str | None = None
    source_commit: str | None = None
    force: bool = False


@dataclass(frozen=True)
class CodexLabAppResult:
    app_dir: Path
    embedded_cli_path: Path
    launcher_path: Path
    shim_path: Path | None


def build_codex_lab_app(options: CodexLabAppOptions) -> CodexLabAppResult:
    if options.source_commit is not None and (
        len(options.source_commit) not in (40, 64)
        or any(
            character not in "0123456789abcdef" for character in options.source_commit
        )
    ):
        raise ValueError(
            "source commit must be a lowercase 40- or 64-character hex SHA"
        )
    codex_bin = options.codex_bin.resolve()
    if not codex_bin.is_file():
        raise FileNotFoundError(f"Codex Lab CLI executable does not exist: {codex_bin}")

    app_dir = options.app_dir.resolve()
    _prepare_output_path(app_dir, options.force)

    contents_dir = app_dir / "Contents"
    macos_dir = contents_dir / "MacOS"
    resources_dir = contents_dir / "Resources"
    macos_dir.mkdir(parents=True)
    resources_dir.mkdir(parents=True)

    embedded_cli_path = resources_dir / EMBEDDED_CLI_NAME
    shutil.copy(codex_bin, embedded_cli_path)
    _make_executable(embedded_cli_path)
    embedded_cli_sha256 = _sha256_file(embedded_cli_path)
    write_codex_lab_icon(resources_dir / ICON_FILE_NAME)

    launcher_path = macos_dir / LAUNCHER_NAME
    with launcher_path.open("w", encoding="utf-8") as handle:
        print(
            _launcher_script(
                embedded_cli_path=embedded_cli_path,
                codex_app_path=options.codex_app_path,
                expected_cli_sha256=embedded_cli_sha256,
                expected_source_commit=options.source_commit,
                expected_cli_version=options.embedded_cli_version
                or options.release_version
                or options.short_version,
                expected_compatibility_version=options.short_version,
            ),
            end="",
            file=handle,
        )
    _make_executable(launcher_path)

    _write_info_plist(contents_dir / "Info.plist", options)
    shim_path = _install_shim(
        options.shim_dir,
        app_dir,
        options.force,
    )

    return CodexLabAppResult(
        app_dir=app_dir,
        embedded_cli_path=embedded_cli_path,
        launcher_path=launcher_path,
        shim_path=shim_path,
    )


def _prepare_output_path(path: Path, force: bool) -> None:
    if not path.exists() and not path.is_symlink():
        return
    if not force:
        raise FileExistsError(f"Output already exists: {path}")
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink()


def _make_executable(path: Path) -> None:
    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _launcher_script(
    *,
    embedded_cli_path: Path,
    codex_app_path: Path,
    expected_cli_sha256: str,
    expected_source_commit: str | None,
    expected_cli_version: str,
    expected_compatibility_version: str,
    codesign_path: Path = Path("/usr/bin/codesign"),
    id_path: Path = Path("/usr/bin/id"),
    launchctl_path: Path = Path("/bin/launchctl"),
    lsappinfo_path: Path = Path("/usr/bin/lsappinfo"),
    lsof_path: Path = Path("/usr/sbin/lsof"),
    open_path: Path = Path("/usr/bin/open"),
    plutil_path: Path = Path("/usr/bin/plutil"),
    ps_path: Path = Path("/bin/ps"),
    shasum_path: Path = Path("/usr/bin/shasum"),
) -> str:
    embedded_cli_name = embedded_cli_path.name
    return f"""#!/bin/sh
set -eu

APP_CONTENTS_DIR=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
LAB_CLI="$APP_CONTENTS_DIR/Resources/{embedded_cli_name}"
CONFIGURED_CODEX_APP={_shell_quote(str(codex_app_path))}
EXPECTED_CLI_SHA256={_shell_quote(expected_cli_sha256)}
EXPECTED_SOURCE_COMMIT={_shell_quote(expected_source_commit or "")}
EXPECTED_CLI_VERSION={_shell_quote(expected_cli_version)}
EXPECTED_COMPATIBILITY_VERSION={_shell_quote(expected_compatibility_version)}
OFFICIAL_BUNDLE_IDENTIFIER={_shell_quote(OFFICIAL_APP_BUNDLE_IDENTIFIER)}
OFFICIAL_TEAM_IDENTIFIER={_shell_quote(OFFICIAL_APP_TEAM_IDENTIFIER)}
SUPERVISOR_LABEL={_shell_quote(APP_SERVER_LABEL)}
SUPERVISOR_RUNNER_RELATIVE_PATH={_shell_quote(str(APP_SERVER_RUNNER_RELATIVE_PATH))}
LISTEN_HOST={_shell_quote(APP_SERVER_LISTEN_HOST)}
LISTEN_PORT={APP_SERVER_LISTEN_PORT}
LISTEN_URL={_shell_quote(APP_SERVER_LISTEN_URL)}
WEBSOCKET_URL={_shell_quote(APP_SERVER_WEBSOCKET_URL)}
CODESIGN={_shell_quote(str(codesign_path))}
ID={_shell_quote(str(id_path))}
LAUNCHCTL={_shell_quote(str(launchctl_path))}
LSAPPINFO={_shell_quote(str(lsappinfo_path))}
LSOF={_shell_quote(str(lsof_path))}
OPEN={_shell_quote(str(open_path))}
PLUTIL={_shell_quote(str(plutil_path))}
PS={_shell_quote(str(ps_path))}
SHASUM={_shell_quote(str(shasum_path))}
if [ -n "${{CODEX_LAB_HOME:-}}" ]; then
  LAB_HOME="$CODEX_LAB_HOME"
elif [ -n "${{HOME:-}}" ]; then
  LAB_HOME="$HOME/.codex-lab"
else
  echo "HOME is unavailable; refusing to resolve the Codex Lab app-server." >&2
  exit 1
fi
home_chatgpt_app=
home_codex_app=
if [ -n "${{HOME:-}}" ]; then
  home_chatgpt_app="$HOME/Applications/ChatGPT.app"
  home_codex_app="$HOME/Applications/Codex.app"
fi
candidate_apps="$CONFIGURED_CODEX_APP
/Applications/ChatGPT.app
$home_chatgpt_app
/Applications/Codex.app
$home_codex_app"

if [ ! -x "$LAB_CLI" ]; then
  echo "Codex Lab CLI is not executable: $LAB_CLI" >&2
  exit 1
fi

actual_cli_sha256=$("$SHASUM" -a 256 "$LAB_CLI" | /usr/bin/awk '{{ print $1 }}')
if [ "$actual_cli_sha256" != "$EXPECTED_CLI_SHA256" ]; then
  echo "Codex Lab CLI checksum does not match the packaged immutable binary." >&2
  exit 1
fi

is_official_codex_app() {{
  candidate=$1
  if [ ! -d "$candidate" ]; then
    return 1
  fi
  bundle_identifier=$("$PLUTIL" \
    -extract CFBundleIdentifier raw -o - \
    "$candidate/Contents/Info.plist" 2>/dev/null || true)
  if [ "$bundle_identifier" != "$OFFICIAL_BUNDLE_IDENTIFIER" ]; then
    return 1
  fi
  if ! "$CODESIGN" --verify --deep --strict "$candidate" >/dev/null 2>&1; then
    return 1
  fi
  team_identifier=$("$CODESIGN" -dv --verbose=4 "$candidate" 2>&1 \
    | /usr/bin/awk -F= '$1 ~ /^TeamIdentifier[[:space:]]*$/ {{ gsub(/^[[:space:]]+|[[:space:]]+$/, "", $2); print $2; exit }}')
  if [ "$team_identifier" != "$OFFICIAL_TEAM_IDENTIFIER" ]; then
    return 1
  fi
  app_executable=$("$PLUTIL" \
    -extract CFBundleExecutable raw -o - \
    "$candidate/Contents/Info.plist" 2>/dev/null || true)
  [ -n "$app_executable" ]
}}

CODEX_APP=
while IFS= read -r candidate; do
  if [ -z "$candidate" ]; then
    continue
  fi
  if is_official_codex_app "$candidate"; then
    CODEX_APP="$candidate"
    break
  fi
done <<EOF
$candidate_apps
EOF

if [ -z "$CODEX_APP" ]; then
  echo "OpenAI coding desktop app was not found." >&2
  echo "Expected bundle identifier: $OFFICIAL_BUNDLE_IDENTIFIER" >&2
  echo "Expected signing team: $OFFICIAL_TEAM_IDENTIFIER" >&2
  echo "Checked:" >&2
  while IFS= read -r candidate; do
    if [ -n "$candidate" ]; then
      echo "  $candidate" >&2
    fi
  done <<EOF
$candidate_apps
EOF
  exit 1
fi

if ! RUNNING_APP_INFO=$("$LSAPPINFO" find bundleid="$OFFICIAL_BUNDLE_IDENTIFIER"); then
  echo "Could not inspect running applications; refusing to launch Codex Lab." >&2
  exit 1
fi
if [ -n "$RUNNING_APP_INFO" ]; then
  echo "OpenAI coding desktop app is already running: $OFFICIAL_BUNDLE_IDENTIFIER" >&2
  echo "Quit it before launching Codex Lab so the websocket environment applies." >&2
  exit 1
fi

PROVENANCE_FILE=$(/usr/bin/mktemp "${{TMPDIR:-/tmp}}/codex-lab-provenance.XXXXXX")
trap '/bin/rm -f "$PROVENANCE_FILE"' EXIT HUP INT TERM
if ! "$LAB_CLI" debug provenance --json >"$PROVENANCE_FILE"; then
  echo "Codex Lab CLI did not report build provenance: $LAB_CLI" >&2
  exit 1
fi
provenance_bytes=$(/usr/bin/wc -c <"$PROVENANCE_FILE" | /usr/bin/tr -d '[:space:]')
case "$provenance_bytes" in
  ''|*[!0-9]*)
    echo "Codex Lab CLI provenance size could not be read: $LAB_CLI" >&2
    exit 1
    ;;
esac
if [ "$provenance_bytes" -eq 0 ] || [ "$provenance_bytes" -gt {MAX_PROVENANCE_BYTES} ]; then
  echo "Codex Lab CLI provenance exceeded the {MAX_PROVENANCE_BYTES}-byte limit: $LAB_CLI" >&2
  exit 1
fi

provenance_field() {{
  "$PLUTIL" -extract "$1" raw -o - "$PROVENANCE_FILE" 2>/dev/null || true
}}
schema_version=$(provenance_field schema_version)
release_version=$(provenance_field release_version)
compatibility_version=$(provenance_field compatibility_version)
source_commit=$(provenance_field source_commit)
dirty_state=$(provenance_field dirty_state)
build_profile=$(provenance_field build_profile)
build_channel=$(provenance_field build_channel)
executable_path=$(provenance_field executable_path)

case "$source_commit" in
  ''|*[!0-9a-f]*)
    echo "Codex Lab CLI source commit is unavailable or malformed." >&2
    exit 1
    ;;
esac
if [ "$schema_version" != "2" ]; then
  echo "Codex Lab CLI provenance schema or source commit is invalid." >&2
  exit 1
fi
case "${{#source_commit}}" in
  40|64) ;;
  *)
    echo "Codex Lab CLI provenance schema or source commit is invalid." >&2
    exit 1
    ;;
esac
if [ -n "$EXPECTED_SOURCE_COMMIT" ] && [ "$source_commit" != "$EXPECTED_SOURCE_COMMIT" ]; then
  echo "Codex Lab CLI source commit does not match the packaged candidate." >&2
  exit 1
fi
if [ "$release_version" != "$EXPECTED_CLI_VERSION" ]; then
  echo "Codex Lab CLI version does not match the packaged candidate." >&2
  exit 1
fi
if [ "$compatibility_version" != "$EXPECTED_COMPATIBILITY_VERSION" ]; then
  echo "Codex Lab CLI compatibility version does not match the packaged candidate." >&2
  exit 1
fi
if [ "$dirty_state" != "clean" ]; then
  echo "Codex Lab CLI was not built from clean tracked source." >&2
  exit 1
fi
if [ -z "$release_version" ] || [ "${{#release_version}}" -gt 128 ] \
  || [ -z "$compatibility_version" ] || [ "${{#compatibility_version}}" -gt 128 ] \
  || [ -z "$build_profile" ] || [ "${{#build_profile}}" -gt 64 ] \
  || [ -z "$build_channel" ] || [ "${{#build_channel}}" -gt 64 ]; then
  echo "Codex Lab CLI build metadata is unavailable or unbounded." >&2
  exit 1
fi
if [ ! "$executable_path" -ef "$LAB_CLI" ]; then
  echo "Codex Lab CLI provenance does not match the embedded executable." >&2
  exit 1
fi
/bin/rm -f "$PROVENANCE_FILE"
trap - EXIT HUP INT TERM

MANAGED_CLI="$LAB_HOME/packages/standalone/current/codex"
if [ ! -x "$MANAGED_CLI" ]; then
  echo "Managed Codex Lab engine is not executable: $MANAGED_CLI" >&2
  exit 1
fi

MANAGED_PROVENANCE_FILE=$(/usr/bin/mktemp "${{TMPDIR:-/tmp}}/codex-lab-managed.XXXXXX")
trap '/bin/rm -f "$MANAGED_PROVENANCE_FILE"' EXIT HUP INT TERM
if ! "$MANAGED_CLI" debug provenance --json >"$MANAGED_PROVENANCE_FILE"; then
  echo "Managed Codex Lab engine did not report build provenance: $MANAGED_CLI" >&2
  exit 1
fi
managed_provenance_bytes=$(/usr/bin/wc -c <"$MANAGED_PROVENANCE_FILE" | /usr/bin/tr -d '[:space:]')
case "$managed_provenance_bytes" in
  ''|*[!0-9]*)
    echo "Managed Codex Lab engine provenance size could not be read: $MANAGED_CLI" >&2
    exit 1
    ;;
esac
if [ "$managed_provenance_bytes" -eq 0 ] || [ "$managed_provenance_bytes" -gt {MAX_PROVENANCE_BYTES} ]; then
  echo "Managed Codex Lab engine provenance exceeded the {MAX_PROVENANCE_BYTES}-byte limit: $MANAGED_CLI" >&2
  exit 1
fi

managed_provenance_field() {{
  "$PLUTIL" -extract "$1" raw -o - "$MANAGED_PROVENANCE_FILE" 2>/dev/null || true
}}
managed_schema_version=$(managed_provenance_field schema_version)
managed_release_version=$(managed_provenance_field release_version)
managed_compatibility_version=$(managed_provenance_field compatibility_version)
managed_source_commit=$(managed_provenance_field source_commit)
managed_dirty_state=$(managed_provenance_field dirty_state)
managed_build_profile=$(managed_provenance_field build_profile)
managed_build_channel=$(managed_provenance_field build_channel)
managed_executable_path=$(managed_provenance_field executable_path)

if [ "$managed_schema_version" != "$schema_version" ] \
  || [ "$managed_release_version" != "$release_version" ] \
  || [ "$managed_compatibility_version" != "$compatibility_version" ] \
  || [ "$managed_source_commit" != "$source_commit" ] \
  || [ "$managed_dirty_state" != "$dirty_state" ] \
  || [ "$managed_build_profile" != "$build_profile" ] \
  || [ "$managed_build_channel" != "$build_channel" ]; then
  echo "Managed Codex Lab engine build does not match the packaged launcher." >&2
  exit 1
fi
if [ ! "$managed_executable_path" -ef "$MANAGED_CLI" ]; then
  echo "Managed Codex Lab engine provenance does not match its executable." >&2
  exit 1
fi
/bin/rm -f "$MANAGED_PROVENANCE_FILE"
trap - EXIT HUP INT TERM

SUPERVISOR_RUNNER="$LAB_HOME/$SUPERVISOR_RUNNER_RELATIVE_PATH"
if [ ! -x "$SUPERVISOR_RUNNER" ]; then
  echo "Codex Lab app-server supervisor is not executable: $SUPERVISOR_RUNNER" >&2
  exit 1
fi
if ! "$SUPERVISOR_RUNNER" check >/dev/null 2>&1; then
  echo "Codex Lab app-server supervisor validation failed under $LAB_HOME." >&2
  exit 1
fi

if ! user_id=$("$ID" -u); then
  echo "Could not resolve the current user for the Codex Lab app-server." >&2
  exit 1
fi
case "$user_id" in ''|*[!0-9]*)
  echo "Current user id is malformed; refusing to inspect the Codex Lab app-server." >&2
  exit 1
;; esac
supervisor_domain="gui/$user_id/$SUPERVISOR_LABEL"
if ! supervisor_state=$("$LAUNCHCTL" print "$supervisor_domain" 2>/dev/null); then
  echo "Codex Lab app-server supervisor is not loaded: $supervisor_domain" >&2
  exit 1
fi
launchd_state=$(printf '%s\n' "$supervisor_state" | /usr/bin/awk '$1 == "state" && $2 == "=" {{ print $3; exit }}')
supervisor_program=$(printf '%s\n' "$supervisor_state" | /usr/bin/awk '$1 == "program" && $2 == "=" {{ sub(/^[^=]*=[[:space:]]*/, ""); print; exit }}')
server_pid=$(printf '%s\n' "$supervisor_state" | /usr/bin/awk '$1 == "pid" && $2 == "=" {{ gsub(/;$/, "", $3); print $3; exit }}')
if [ "$launchd_state" != "running" ]; then
  echo "Codex Lab app-server supervisor is not running: $supervisor_domain" >&2
  exit 1
fi
if [ -z "$supervisor_program" ] || [ ! "$supervisor_program" -ef "$SUPERVISOR_RUNNER" ]; then
  echo "Codex Lab launchd service does not use the managed supervisor runner." >&2
  exit 1
fi
case "$server_pid" in ''|*[!0-9]*)
  echo "Codex Lab launchd service did not report a valid app-server pid." >&2
  exit 1
;; esac
if [ "$server_pid" -le 0 ] || ! /bin/kill -0 "$server_pid" 2>/dev/null; then
  echo "Codex Lab launchd app-server pid is not alive: $server_pid" >&2
  exit 1
fi
server_executable=$("$LSOF" -a -p "$server_pid" -d txt -Fn 2>/dev/null \
  | /usr/bin/awk 'substr($0, 1, 1) == "n" {{ print substr($0, 2); exit }}')
if [ -z "$server_executable" ] || [ ! "$server_executable" -ef "$MANAGED_CLI" ]; then
  echo "Codex Lab launchd app-server does not execute the managed engine." >&2
  exit 1
fi
if ! server_command=$("$PS" -ww -p "$server_pid" -o command= 2>/dev/null); then
  echo "Could not inspect the Codex Lab launchd app-server command." >&2
  exit 1
fi
expected_server_command="$MANAGED_CLI app-server --remote-control --listen $LISTEN_URL"
if [ "$server_command" != "$expected_server_command" ]; then
  echo "Codex Lab launchd app-server command does not match the pinned websocket service." >&2
  exit 1
fi
if ! "$LSOF" -nP -a -p "$server_pid" \
  -iTCP@"$LISTEN_HOST":"$LISTEN_PORT" -sTCP:LISTEN -Fp 2>/dev/null \
  | /usr/bin/grep -qx "p$server_pid"; then
  echo "Codex Lab launchd app-server does not own $LISTEN_URL." >&2
  exit 1
fi

echo "Selected OpenAI coding desktop app: $CODEX_APP" >&2
echo "Codex Lab CLI provenance: commit=$source_commit dirty=$dirty_state profile=$build_profile channel=$build_channel release=$release_version compatibility=$compatibility_version" >&2
unset CODEX_CLI_PATH CODEX_APP_SERVER_FORCE_CLI CODEX_APP_SERVER_USE_LOCAL_DAEMON CODEX_APP_SERVER_WS_URL
exec "$OPEN" -n \
  --env "CODEX_CLI_PATH=" \
  --env "CODEX_APP_SERVER_FORCE_CLI=" \
  --env "CODEX_HOME=$LAB_HOME" \
  --env "CODEX_LAB_HOME=$LAB_HOME" \
  --env "CODEX_APP_SERVER_USE_LOCAL_DAEMON=" \
  --env "CODEX_APP_SERVER_WS_URL=$WEBSOCKET_URL" \
  "$CODEX_APP"
"""


def _shell_quote(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


def _write_info_plist(path: Path, options: CodexLabAppOptions) -> None:
    info = {
        "CFBundleDisplayName": options.display_name,
        "CFBundleExecutable": LAUNCHER_NAME,
        "CFBundleIconFile": ICON_FILE_NAME,
        "CFBundleIdentifier": options.bundle_identifier,
        "CFBundleName": options.display_name,
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": options.short_version,
        "CFBundleVersion": options.bundle_version,
        "CodexLabReleaseVersion": options.release_version
        or options.embedded_cli_version
        or options.short_version,
        "LSMinimumSystemVersion": "13.0",
        "NSHighResolutionCapable": True,
    }
    with path.open("wb") as handle:
        plistlib.dump(info, handle, sort_keys=True)


def _install_shim(
    shim_dir: Path | None,
    app_dir: Path,
    force: bool,
) -> Path | None:
    if shim_dir is None:
        return None

    shim_dir.mkdir(parents=True, exist_ok=True)
    shim_path = shim_dir / SHIM_NAME
    if shim_path.exists() or shim_path.is_symlink():
        if not force:
            raise FileExistsError(f"Shim already exists: {shim_path}")
        _prepare_output_path(shim_path, force)

    with shim_path.open("w", encoding="utf-8") as handle:
        print(build_shim_script(app_dir), end="", file=handle)
    _make_executable(shim_path)
    return shim_path


def build_shim_script(app_dir: Path) -> str:
    """Return the codex-lab shell shim for an installed app bundle."""

    return f"""#!/bin/sh
set -eu

SHIM_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
built_app={_shell_quote(str(app_dir))}
home_app="${{HOME:+$HOME/Applications/Codex Lab.app}}"
candidate_apps="${{CODEX_LAB_APP_PATH:-}}
$SHIM_DIR/../Codex Lab.app
$built_app
/Applications/Codex Lab.app
$home_app"

LAB_CLI=
while IFS= read -r app_path; do
  if [ -z "$app_path" ]; then
    continue
  fi
  candidate_cli="$app_path/Contents/Resources/codex-lab"
  if [ -x "$candidate_cli" ]; then
    LAB_CLI="$candidate_cli"
    break
  fi
done <<EOF
$candidate_apps
EOF

if [ -z "$LAB_CLI" ]; then
  echo "Codex Lab CLI was not found. Install Codex Lab.app in /Applications or set CODEX_LAB_APP_PATH." >&2
  exit 1
fi

exec "$LAB_CLI" "$@"
"""
