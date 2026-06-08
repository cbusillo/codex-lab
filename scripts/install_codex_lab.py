#!/usr/bin/env python3
"""Install Codex Lab from a published distribution manifest."""

from pathlib import Path
import argparse
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))

from codex_lab_package.installer import DEFAULT_APP_DIR
from codex_lab_package.installer import DEFAULT_REPOSITORY
from codex_lab_package.installer import DEFAULT_SHIM_DIR
from codex_lab_package.installer import DEFAULT_STATE_PATH
from codex_lab_package.installer import install_from_manifest_url
from codex_lab_package.installer import manifest_url_for_release_tag


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Install Codex Lab app and CLI shim from a release manifest.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument(
        "--manifest-url", help="Published codex-lab-distribution.json URL."
    )
    source.add_argument("--release-tag", help="Codex Lab release tag to install.")
    parser.add_argument(
        "--repository",
        default=DEFAULT_REPOSITORY,
        help="GitHub OWNER/REPO used with --release-tag.",
    )
    parser.add_argument(
        "--app-dir",
        type=Path,
        default=DEFAULT_APP_DIR,
        help="Destination Codex Lab.app path.",
    )
    parser.add_argument(
        "--shim-dir",
        type=Path,
        default=DEFAULT_SHIM_DIR,
        help="Destination directory for the codex-lab shim.",
    )
    parser.add_argument(
        "--no-shim",
        action="store_true",
        help="Install only Codex Lab.app and skip the codex-lab shim.",
    )
    parser.add_argument(
        "--state-path",
        type=Path,
        default=DEFAULT_STATE_PATH,
        help="Installer state JSON path.",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Replace an existing app bundle or shim after verification succeeds.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    manifest_url = args.manifest_url or manifest_url_for_release_tag(
        args.release_tag,
        repository=args.repository,
    )
    result = install_from_manifest_url(
        manifest_url,
        app_dir=args.app_dir,
        shim_dir=None if args.no_shim else args.shim_dir,
        state_path=args.state_path,
        force=args.force,
    )
    print(f"Installed Codex Lab {result.version} from {result.release_tag}")
    print(f"App: {result.app_dir}")
    if result.shim_path is not None:
        print(f"Shim: {result.shim_path}")
    print(f"State: {result.state_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
