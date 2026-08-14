"""Smoke checks for generated Codex Lab launcher bundles."""

import argparse
import plistlib
import subprocess
from pathlib import Path
import sys


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_lab_package.icon import ICON_FILE_NAME
from codex_lab_package.layout import MAX_PROVENANCE_BYTES
from codex_lab_package.live_smoke import read_cli_provenance


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
    parser.add_argument(
        "--expected-source-commit",
        help="Expected source commit reported by the embedded CLI.",
    )
    parser.add_argument("--expected-release-version")
    parser.add_argument("--expected-compatibility-version")
    parser.add_argument("--expected-bundle-version")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    smoke_check(
        args.app_dir.resolve(),
        args.shim_path.resolve() if args.shim_path else None,
        args.expected_source_commit,
        args.expected_release_version,
        args.expected_compatibility_version,
        args.expected_bundle_version,
    )
    return 0


def smoke_check(
    app_dir: Path,
    shim_path: Path | None,
    expected_source_commit: str | None = None,
    expected_release_version: str | None = None,
    expected_compatibility_version: str | None = None,
    expected_bundle_version: str | None = None,
) -> None:
    contents_dir = app_dir / "Contents"
    launcher_path = contents_dir / "MacOS" / "Codex Lab Launcher"
    embedded_cli_path = contents_dir / "Resources" / "codex-lab"
    icon_path = contents_dir / "Resources" / ICON_FILE_NAME
    info_plist_path = contents_dir / "Info.plist"

    _require_directory(app_dir)
    _require_executable(launcher_path)
    _require_executable(embedded_cli_path)
    _require_file(icon_path)
    _check_icns(icon_path)
    _require_file(info_plist_path)
    _check_plist(
        info_plist_path,
        expected_release_version=expected_release_version,
        expected_compatibility_version=expected_compatibility_version,
        expected_bundle_version=expected_bundle_version,
    )
    _check_shell_syntax(launcher_path)

    if (
        expected_source_commit is not None
        or expected_release_version is not None
        or expected_compatibility_version is not None
    ):
        provenance = read_cli_provenance(embedded_cli_path)
        if (
            expected_source_commit is not None
            and provenance["source_commit"] != expected_source_commit
        ):
            raise ValueError(
                "embedded CLI source commit does not match the expected package commit"
            )
        if (
            expected_release_version is not None
            and provenance["release_version"] != expected_release_version
        ):
            raise ValueError(
                "embedded CLI release version does not match the expected package release"
            )
        if (
            expected_compatibility_version is not None
            and provenance["compatibility_version"] != expected_compatibility_version
        ):
            raise ValueError(
                "embedded CLI compatibility version does not match the expected package version"
            )

    launcher = launcher_path.read_text(encoding="utf-8")
    _require_contains(
        launcher,
        "unset CODEX_CLI_PATH CODEX_APP_SERVER_FORCE_CLI CODEX_APP_SERVER_USE_LOCAL_DAEMON CODEX_APP_SERVER_WS_URL",
        launcher_path,
    )
    _require_contains(launcher, '--env "CODEX_CLI_PATH="', launcher_path)
    _require_contains(launcher, '--env "CODEX_APP_SERVER_FORCE_CLI="', launcher_path)
    _require_not_contains(launcher, '--env "CODEX_CLI_PATH=$LAB_CLI"', launcher_path)
    _require_contains(launcher, "CODEX_HOME=$LAB_HOME", launcher_path)
    _require_contains(launcher, "CODEX_LAB_HOME=$LAB_HOME", launcher_path)
    _require_contains(launcher, "CODEX_APP_SERVER_USE_LOCAL_DAEMON=", launcher_path)
    _require_contains(launcher, "CODEX_APP_SERVER_WS_URL=$WEBSOCKET_URL", launcher_path)
    _require_contains(launcher, "ws://127.0.0.1:4766/rpc", launcher_path)
    _require_contains(launcher, "dev.everycode.codex-lab.app-server.v1", launcher_path)
    _require_contains(launcher, "launchctl", launcher_path)
    _require_contains(launcher, "supervisor/v1/codex-lab-app-server", launcher_path)
    _require_contains(launcher, "-sTCP:LISTEN", launcher_path)
    _require_not_contains(
        launcher, "CODEX_APP_SERVER_USE_LOCAL_DAEMON=1", launcher_path
    )
    _require_not_contains(launcher, "app-server daemon version", launcher_path)
    _require_contains(launcher, "packages/standalone/current/codex", launcher_path)
    _require_contains(
        launcher, "Managed Codex Lab engine build does not match", launcher_path
    )
    _require_contains(launcher, "Resources/codex-lab", launcher_path)
    _require_contains(launcher, "EXPECTED_CLI_SHA256=", launcher_path)
    _require_contains(launcher, "EXPECTED_CLI_VERSION=", launcher_path)
    _require_contains(launcher, "EXPECTED_SOURCE_COMMIT=", launcher_path)
    _require_contains(launcher, "/Applications/ChatGPT.app", launcher_path)
    _require_contains(launcher, "$HOME/Applications/ChatGPT.app", launcher_path)
    _require_contains(launcher, "/Applications/Codex.app", launcher_path)
    _require_contains(launcher, "$HOME/Applications/Codex.app", launcher_path)
    _require_contains(launcher, "com.openai.codex", launcher_path)
    _require_contains(launcher, "codesign", launcher_path)
    _require_contains(launcher, "lsappinfo", launcher_path)
    _require_contains(launcher, "debug provenance --json", launcher_path)
    _require_contains(launcher, f"{MAX_PROVENANCE_BYTES}-byte limit", launcher_path)
    _require_contains(launcher, "Quit it before launching Codex Lab", launcher_path)
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


def _check_plist(
    path: Path,
    *,
    expected_release_version: str | None = None,
    expected_compatibility_version: str | None = None,
    expected_bundle_version: str | None = None,
) -> None:
    with path.open("rb") as handle:
        info = plistlib.load(handle)

    expected = {
        "CFBundleDisplayName": "Codex Lab",
        "CFBundleExecutable": "Codex Lab Launcher",
        "CFBundleIconFile": ICON_FILE_NAME,
        "CFBundleName": "Codex Lab",
        "CFBundlePackageType": "APPL",
    }
    for key, value in expected.items():
        if info.get(key) != value:
            raise ValueError(f"{path}: expected {key}={value!r}, got {info.get(key)!r}")
    for key, value in (
        ("CodexLabReleaseVersion", expected_release_version),
        ("CFBundleShortVersionString", expected_compatibility_version),
        ("CFBundleVersion", expected_bundle_version),
    ):
        if value is not None and info.get(key) != value:
            raise ValueError(f"{path}: expected {key}={value!r}, got {info.get(key)!r}")


def _check_icns(path: Path) -> None:
    contents = path.read_bytes()
    if len(contents) < 8 or contents[:4] != b"icns":
        raise ValueError(f"{path}: invalid ICNS header")
    if int.from_bytes(contents[4:8], "big") != len(contents):
        raise ValueError(f"{path}: invalid ICNS length")


def _check_shell_syntax(path: Path) -> None:
    subprocess.run(["sh", "-n", str(path)], check=True)


def _require_contains(contents: str, needle: str, path: Path) -> None:
    if needle not in contents:
        raise ValueError(f"{path}: missing {needle!r}")


def _require_not_contains(contents: str, needle: str, path: Path) -> None:
    if needle in contents:
        raise ValueError(f"{path}: unexpected {needle!r}")


if __name__ == "__main__":
    raise SystemExit(main())
