"""Command-line interface for building Codex Lab launcher bundles."""

import argparse
import tempfile
from pathlib import Path

from .layout import DEFAULT_BUNDLE_IDENTIFIER
from .layout import DEFAULT_CODEX_APP_PATH
from .layout import DEFAULT_DISPLAY_NAME
from .layout import CodexLabAppOptions
from .layout import build_codex_lab_app


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
        help="Official Codex Desktop app path to launch.",
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
        default="0.0.0",
        help="CFBundleShortVersionString for the launcher bundle.",
    )
    parser.add_argument(
        "--bundle-version",
        default="1",
        help="CFBundleVersion for the launcher bundle.",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Replace an existing app bundle or shim.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    app_dir_arg = getattr(args, "app_dir", None)
    app_dir = (
        app_dir_arg.resolve()
        if app_dir_arg is not None
        else Path(tempfile.mkdtemp(prefix="codex-lab-app-")) / "Codex Lab.app"
    )

    result = build_codex_lab_app(
        CodexLabAppOptions(
            app_dir=app_dir,
            codex_bin=args.codex_bin.resolve(),
            codex_app_path=args.codex_app_path,
            shim_dir=args.shim_dir.resolve() if args.shim_dir else None,
            bundle_identifier=args.bundle_id,
            display_name=args.display_name,
            short_version=args.short_version,
            bundle_version=args.bundle_version,
            force=args.force,
        )
    )

    print(f"Built Codex Lab app bundle at {result.app_dir}")
    if result.shim_path is not None:
        print(f"Installed codex-lab shim at {result.shim_path}")
    return 0
