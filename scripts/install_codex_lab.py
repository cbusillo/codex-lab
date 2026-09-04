#!/usr/bin/env python3
"""Install Codex Lab from a published distribution manifest."""

from pathlib import Path
import argparse
import subprocess
import sys
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))

from codex_lab_package.installer import DEFAULT_APP_DIR
from codex_lab_package.installer import DEFAULT_REPOSITORY
from codex_lab_package.installer import DEFAULT_SHIM_DIR
from codex_lab_package.installer import DEFAULT_STATE_PATH
from codex_lab_package.installer import CodexLabInstallStateError
from codex_lab_package.installer import CodexLabRollbackError
from codex_lab_package.installer import CodexLabUpdateError
from codex_lab_package.installer import activate_code_route
from codex_lab_package.installer import check_for_update
from codex_lab_package.installer import deactivate_code_route
from codex_lab_package.installer import install_from_manifest_url
from codex_lab_package.installer import manifest_url_for_latest_release
from codex_lab_package.installer import manifest_url_for_release_tag
from codex_lab_package.installer import read_install_state
from codex_lab_package.installer import require_recorded_code_route
from codex_lab_package.installer import update_from_latest_release
from codex_lab_package.installer import uninstall_codex_lab


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Install Codex Lab app, CLI shim, signed engine, and supervisor.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument(
        "--manifest-url", help="Published codex-lab-distribution.json URL."
    )
    source.add_argument("--release-tag", help="Codex Lab release tag to install.")
    source.add_argument(
        "--latest",
        action="store_true",
        help="Install the newest published Codex Lab release for the repository.",
    )
    source.add_argument(
        "--status",
        action="store_true",
        help="Print installed Codex Lab release metadata from the state file.",
    )
    source.add_argument(
        "--check",
        action="store_true",
        help="Check whether the installed Codex Lab release is current.",
    )
    source.add_argument(
        "--update",
        action="store_true",
        help=(
            "Update the recorded Codex Lab install after the explicit code route "
            "has been deactivated."
        ),
    )
    source.add_argument(
        "--uninstall",
        action="store_true",
        help="Remove the recorded Codex Lab install and restore a prior managed engine.",
    )
    source.add_argument(
        "--activate-code",
        action="store_true",
        help="Explicitly route ~/.local/bin/code to the verified managed Codex Lab engine.",
    )
    source.add_argument(
        "--deactivate-code",
        action="store_true",
        help="Restore the route captured by --activate-code.",
    )
    parser.add_argument(
        "--repository",
        default=DEFAULT_REPOSITORY,
        help="GitHub OWNER/REPO used with --release-tag or --latest.",
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
        help=(
            "Replace existing app, shim, or engine paths after verification succeeds; "
            "does not override an active code route."
        ),
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.status:
        try:
            status = read_install_state(args.state_path)
            require_recorded_code_route(status)
        except CodexLabInstallStateError as exc:
            return print_install_state_error(exc)
        except ValueError as exc:
            return print_command_error("Code route is invalid", exc)
        print(f"Codex Lab {status.version} from {status.release_tag}")
        print(f"Release version: {status.release_version}")
        print(f"Bundle version: {status.bundle_version}")
        if status.source_commit:
            print(f"Source commit: {status.source_commit}")
        print(f"App: {status.app_path}")
        if status.shim_path is not None:
            print(f"Shim: {status.shim_path}")
        else:
            print("Shim: not installed")
        if status.code_route is None:
            print("Code route: inactive")
        else:
            print(f"Code route: active at {status.code_route.active_path}")
            print(f"Code route engine: {status.code_route.engine.path}")
            print(f"Code route release: {status.code_route.engine.release_tag}")
            print(
                "Code route release version: "
                f"{status.code_route.engine.release_version}"
            )
            print(f"Code route prior: {status.code_route.prior.kind}")
        if status.engine_path is not None:
            print(f"Engine: {status.engine_path}")
        if status.supervisor_label is not None:
            print(f"Supervisor: {status.supervisor_label}")
        print(f"State: {status.state_path}")
        return 0

    if args.uninstall:
        try:
            uninstall_result = uninstall_codex_lab(state_path=args.state_path)
        except CodexLabInstallStateError as exc:
            return print_install_state_error(exc)
        except (CodexLabRollbackError, OSError, ValueError) as exc:
            return print_command_error("Could not uninstall Codex Lab", exc)
        print("Uninstalled Codex Lab")
        print(f"App: {uninstall_result.app_path}")
        if uninstall_result.shim_path is not None:
            print(f"Shim: {uninstall_result.shim_path}")
        if uninstall_result.engine_path is not None:
            print(f"Engine: {uninstall_result.engine_path}")
        if uninstall_result.restored_engine_path is not None:
            print(f"Restored prior engine: {uninstall_result.restored_engine_path}")
        if uninstall_result.restored_code_route_path is not None:
            print(
                "Restored prior code route: "
                f"{uninstall_result.restored_code_route_path}"
            )
        print(f"State: {uninstall_result.state_path}")
        return 0

    if args.activate_code:
        try:
            route_result = activate_code_route(state_path=args.state_path)
        except CodexLabInstallStateError as exc:
            return print_install_state_error(exc)
        except (OSError, ValueError) as exc:
            return print_command_error("Could not activate the code route", exc)
        if route_result.changed:
            print(f"Activated code route: {route_result.active_path}")
        else:
            print(f"Code route is already active: {route_result.active_path}")
        print(f"State: {route_result.state_path}")
        return 0

    if args.deactivate_code:
        try:
            route_result = deactivate_code_route(state_path=args.state_path)
        except CodexLabInstallStateError as exc:
            return print_install_state_error(exc)
        except (OSError, ValueError) as exc:
            return print_command_error("Could not deactivate the code route", exc)
        if not route_result.changed:
            print("Code route is already inactive")
        elif route_result.restored_prior:
            print(f"Restored prior code route: {route_result.active_path}")
        else:
            print(f"Removed code route: {route_result.active_path}")
        print(f"State: {route_result.state_path}")
        return 0

    if args.check:
        try:
            check = check_for_update(
                repository=args.repository,
                state_path=args.state_path,
            )
        except CodexLabInstallStateError as exc:
            return print_install_state_error(exc)
        except (CodexLabUpdateError, OSError, ValueError) as exc:
            return print_command_error("Could not check Codex Lab updates", exc)
        if check.update_available:
            print(
                "Codex Lab update available: "
                f"{check.installed.release_tag} -> {check.latest_release_tag}"
            )
        else:
            print(f"Codex Lab is up to date: {check.installed.release_tag}")
        return 0

    if args.update:
        try:
            update = update_from_latest_release(
                repository=args.repository,
                state_path=args.state_path,
            )
        except CodexLabInstallStateError as exc:
            return print_install_state_error(exc)
        except (
            CodexLabUpdateError,
            CodexLabRollbackError,
            OSError,
            subprocess.CalledProcessError,
            ValueError,
            zipfile.BadZipFile,
        ) as exc:
            return print_command_error("Could not update Codex Lab", exc)
        if update.install is None:
            print(f"Codex Lab is up to date: {update.check.installed.release_tag}")
            return 0
        print(
            "Updated Codex Lab "
            f"{update.check.installed.release_tag} -> {update.install.release_tag}"
        )
        print(f"App: {update.install.app_dir}")
        if update.install.shim_path is not None:
            print(f"Shim: {update.install.shim_path}")
        print(f"State: {update.install.state_path}")
        return 0

    try:
        if args.manifest_url:
            manifest_url = args.manifest_url
        elif args.latest:
            manifest_url = manifest_url_for_latest_release(repository=args.repository)
        else:
            manifest_url = manifest_url_for_release_tag(
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
    except (
        OSError,
        subprocess.CalledProcessError,
        CodexLabRollbackError,
        ValueError,
        zipfile.BadZipFile,
    ) as exc:
        return print_command_error("Could not install Codex Lab", exc)
    print(f"Installed Codex Lab {result.version} from {result.release_tag}")
    print(f"App: {result.app_dir}")
    print(f"Engine: {result.engine_path}")
    print(f"Supervisor: {result.supervisor_label}")
    if result.shim_path is not None:
        print(f"Shim: {result.shim_path}")
    print(f"State: {result.state_path}")
    return 0


def print_install_state_error(exc: BaseException) -> int:
    print(str(exc), file=sys.stderr)
    return 1


def print_command_error(prefix: str, exc: BaseException) -> int:
    print(f"{prefix}: {exc}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
