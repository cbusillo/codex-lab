"""Command-line interface for building Codex Lab launcher bundles."""

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path

from .layout import DEFAULT_BUNDLE_IDENTIFIER
from .layout import DEFAULT_CODEX_APP_PATH
from .layout import DEFAULT_DISPLAY_NAME
from .layout import CodexLabAppOptions
from .layout import build_codex_lab_app
from .live_smoke import read_cli_provenance


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build a Codex Lab macOS launcher app bundle.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--codex-bin",
        type=Path,
        required=True,
        help="Prebuilt Codex Lab CLI executable to embed in the app bundle.",
    )
    parser.add_argument(
        "--app-dir",
        type=Path,
        default=argparse.SUPPRESS,
        help=(
            "Output .app directory. Defaults to a new temporary "
            "directory named Codex Lab.app."
        ),
    )
    parser.add_argument(
        "--shim-dir",
        type=Path,
        help="Optional directory where a codex-lab wrapper should be installed.",
    )
    parser.add_argument(
        "--codex-app-path",
        type=Path,
        default=DEFAULT_CODEX_APP_PATH,
        help=(
            "Preferred official OpenAI coding desktop app path. The generated "
            "launcher also checks ChatGPT.app and legacy Codex.app in "
            "/Applications and ~/Applications."
        ),
    )
    parser.add_argument(
        "--bundle-id",
        default=DEFAULT_BUNDLE_IDENTIFIER,
        help="CFBundleIdentifier for the launcher bundle.",
    )
    parser.add_argument(
        "--display-name",
        default=DEFAULT_DISPLAY_NAME,
        help="Display name for the launcher bundle.",
    )
    parser.add_argument(
        "--short-version",
        help="CFBundleShortVersionString. Defaults to the embedded CLI version.",
    )
    parser.add_argument(
        "--bundle-version",
        default="1",
        help="CFBundleVersion for the launcher bundle.",
    )
    parser.add_argument(
        "--source-commit",
        help="Expected 40- or 64-character source commit for the embedded CLI.",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Replace an existing app bundle or shim.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    codex_bin = args.codex_bin.resolve()
    try:
        provenance = read_cli_provenance(codex_bin)
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        print(f"Could not inspect Codex Lab CLI provenance: {error}", file=sys.stderr)
        return 2

    short_version = args.short_version or provenance["version"]
    if short_version != provenance["version"]:
        print(
            "Requested short version does not match the embedded CLI provenance: "
            f"{short_version} != {provenance['version']}",
            file=sys.stderr,
        )
        return 2
    source_commit = args.source_commit or provenance["source_commit"]
    if source_commit != provenance["source_commit"]:
        print(
            "Requested source commit does not match the embedded CLI provenance: "
            f"{source_commit} != {provenance['source_commit']}",
            file=sys.stderr,
        )
        return 2

    app_dir_arg = getattr(args, "app_dir", None)
    app_dir = (
        app_dir_arg.resolve()
        if app_dir_arg is not None
        else Path(tempfile.mkdtemp(prefix="codex-lab-app-")) / "Codex Lab.app"
    )

    result = build_codex_lab_app(
        CodexLabAppOptions(
            app_dir=app_dir,
            codex_bin=codex_bin,
            codex_app_path=args.codex_app_path,
            shim_dir=args.shim_dir.resolve() if args.shim_dir else None,
            bundle_identifier=args.bundle_id,
            display_name=args.display_name,
            short_version=short_version,
            bundle_version=args.bundle_version,
            source_commit=source_commit,
            force=args.force,
        )
    )

    print(f"Built Codex Lab app bundle at {result.app_dir}")
    if result.shim_path is not None:
        print(f"Installed codex-lab shim at {result.shim_path}")
    return 0
