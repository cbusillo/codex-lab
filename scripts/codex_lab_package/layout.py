"""Build the macOS Codex Lab launcher app bundle layout."""

from dataclasses import dataclass
from pathlib import Path
import plistlib
import shutil
import stat


DEFAULT_BUNDLE_IDENTIFIER = "dev.everycode.codex-lab"
DEFAULT_CODEX_APP_PATH = Path("/Applications/Codex.app")
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
    bundle_version: str = "1"
    force: bool = False


@dataclass(frozen=True)
class CodexLabAppResult:
    app_dir: Path
    embedded_cli_path: Path
    launcher_path: Path
    shim_path: Path | None


def build_codex_lab_app(options: CodexLabAppOptions) -> CodexLabAppResult:
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

    launcher_path = macos_dir / LAUNCHER_NAME
    with launcher_path.open("w", encoding="utf-8") as handle:
        print(
            _launcher_script(
                embedded_cli_path=embedded_cli_path,
                codex_app_path=options.codex_app_path,
            ),
            end="",
            file=handle,
        )
    _make_executable(launcher_path)

    _write_info_plist(contents_dir / "Info.plist", options)
    shim_path = _install_shim(
        options.shim_dir,
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


def _launcher_script(*, embedded_cli_path: Path, codex_app_path: Path) -> str:
    embedded_cli_name = embedded_cli_path.name
    return f"""#!/bin/sh
set -eu

APP_CONTENTS_DIR=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
LAB_CLI="$APP_CONTENTS_DIR/Resources/{embedded_cli_name}"
CODEX_APP={_shell_quote(str(codex_app_path))}

if [ ! -x "$LAB_CLI" ]; then
  echo "Codex Lab CLI is not executable: $LAB_CLI" >&2
  exit 1
fi

if [ ! -d "$CODEX_APP" ]; then
  echo "Codex Desktop app was not found: $CODEX_APP" >&2
  exit 1
fi

export CODEX_CLI_PATH="$LAB_CLI"
exec open -n --env "CODEX_CLI_PATH=$LAB_CLI" "$CODEX_APP"
"""


def _shell_quote(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


def _write_info_plist(path: Path, options: CodexLabAppOptions) -> None:
    info = {
        "CFBundleDisplayName": options.display_name,
        "CFBundleExecutable": LAUNCHER_NAME,
        "CFBundleIdentifier": options.bundle_identifier,
        "CFBundleName": options.display_name,
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": options.short_version,
        "CFBundleVersion": options.bundle_version,
        "LSMinimumSystemVersion": "13.0",
        "NSHighResolutionCapable": True,
    }
    with path.open("wb") as handle:
        plistlib.dump(info, handle, sort_keys=True)


def _install_shim(
    shim_dir: Path | None,
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
        print(_shim_script(), end="", file=handle)
    _make_executable(shim_path)
    return shim_path


def _shim_script() -> str:
    return """#!/bin/sh
set -eu

candidate_apps="${CODEX_LAB_APP_PATH:-}
/Applications/Codex Lab.app
$HOME/Applications/Codex Lab.app"

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
