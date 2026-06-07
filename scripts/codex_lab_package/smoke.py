"""Smoke checks for generated Codex Lab launcher bundles."""

import argparse
import plistlib
import subprocess
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Smoke check a generated Codex Lab app bundle.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("app_dir", type=Path, help="Codex Lab.app directory to check.")
    parser.add_argument(
        "--shim-path",
        type=Path,
        help="Optional codex-lab terminal wrapper path to check.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    smoke_check(
        args.app_dir.resolve(), args.shim_path.resolve() if args.shim_path else None
    )
    return 0


def smoke_check(app_dir: Path, shim_path: Path | None) -> None:
    contents_dir = app_dir / "Contents"
    launcher_path = contents_dir / "MacOS" / "Codex Lab Launcher"
    embedded_cli_path = contents_dir / "Resources" / "codex-lab"
    info_plist_path = contents_dir / "Info.plist"

    _require_directory(app_dir)
    _require_executable(launcher_path)
    _require_executable(embedded_cli_path)
    _require_file(info_plist_path)
    _check_plist(info_plist_path)
    _check_shell_syntax(launcher_path)

    launcher = launcher_path.read_text(encoding="utf-8")
    _require_contains(launcher, "CODEX_CLI_PATH", launcher_path)
    _require_contains(launcher, "Resources/codex-lab", launcher_path)
    _require_contains(launcher, "open -n", launcher_path)
    _require_contains(launcher, "--env", launcher_path)

    if shim_path is not None:
        _require_executable(shim_path)
        _check_shell_syntax(shim_path)
        shim = shim_path.read_text(encoding="utf-8")
        _require_contains(shim, "../Codex Lab.app", shim_path)
        _require_contains(shim, "built_app=", shim_path)
        _require_contains(shim, "CODEX_LAB_APP_PATH", shim_path)
        _require_contains(shim, "/Applications/Codex Lab.app", shim_path)


def _require_directory(path: Path) -> None:
    if not path.is_dir():
        raise FileNotFoundError(f"Expected directory: {path}")


def _require_file(path: Path) -> None:
    if not path.is_file():
        raise FileNotFoundError(f"Expected file: {path}")


def _require_executable(path: Path) -> None:
    _require_file(path)
    if not path.stat().st_mode & 0o111:
        raise PermissionError(f"Expected executable file: {path}")


def _check_plist(path: Path) -> None:
    with path.open("rb") as handle:
        info = plistlib.load(handle)

    expected = {
        "CFBundleDisplayName": "Codex Lab",
        "CFBundleExecutable": "Codex Lab Launcher",
        "CFBundleName": "Codex Lab",
        "CFBundlePackageType": "APPL",
    }
    for key, value in expected.items():
        if info.get(key) != value:
            raise ValueError(f"{path}: expected {key}={value!r}, got {info.get(key)!r}")


def _check_shell_syntax(path: Path) -> None:
    subprocess.run(["sh", "-n", str(path)], check=True)


def _require_contains(contents: str, needle: str, path: Path) -> None:
    if needle not in contents:
        raise ValueError(f"{path}: missing {needle!r}")


if __name__ == "__main__":
    raise SystemExit(main())
