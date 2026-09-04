"""Install Codex Lab release artifacts from a distribution manifest."""

from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
import json
import shutil
import stat
import subprocess
import tempfile
import urllib.parse
import urllib.request
import uuid
import zipfile

from .code_route import DEFAULT_CODE_ROUTE_PATH
from .code_route import CodeRouteEngine
from .code_route import CodeRouteResult
from .code_route import CodeRouteState
from .code_route import activate_code_route as activate_installed_code_route
from .code_route import deactivate_code_route as deactivate_installed_code_route
from .code_route import read_code_route_state
from .code_route import recover_code_route_transaction
from .code_route import require_active_code_route
from .code_route import serialize_code_route_state
from .code_route_transaction import transaction_lock
from .distribution_manifest import APP_ZIP
from .distribution_manifest import ARTIFACTS
from .distribution_manifest import ENGINE_ZIP
from .distribution_manifest import MANIFEST_NAME
from .distribution_manifest import SHIM_ZIP
from .distribution_manifest import is_https_url
from .distribution_manifest import read_sha256sums
from .distribution_manifest import sha256_file
from .distribution_manifest import validate_manifest
from .engine_contract import CODE_MODE_HOST_ARCHIVE_PATH
from .engine_contract import ENGINE_CLI_ARCHIVE_PATH
from .layout import build_shim_script
from .release_tag import LAB_RELEASE_TAG_PREFIX
from .release_tag import codex_lab_release_order
from .release_tag import release_identity_from_tag
from .smoke import smoke_check
from .supervisor import CodeModeHostIdentity
from .supervisor import EngineIdentity
from .supervisor import SupervisorPaths
from .supervisor import default_supervisor_paths
from .supervisor import inspect_engine
from .supervisor import inspect_code_mode_host
from .supervisor import install_supervisor
from .supervisor import uninstall_supervisor


DEFAULT_REPOSITORY = "cbusillo/codex-lab"
DEFAULT_APP_DIR = Path.home() / "Applications" / "Codex Lab.app"
DEFAULT_SHIM_DIR = Path.home() / ".local" / "bin"
DEFAULT_STATE_PATH = (
    Path.home() / "Library" / "Application Support" / "Codex Lab" / "install-state.json"
)
SHA256SUMS_NAME = "SHA256SUMS"
USER_AGENT = "codex-lab-installer/0"
DOWNLOAD_TIMEOUT_SECONDS = 60
MAX_LATEST_RELEASE_PAGES = 10

DownloadFunc = Callable[[str, Path], None]


class CodexLabInstallStateError(Exception):
    pass


class CodexLabUpdateError(Exception):
    pass


class CodexLabRollbackError(Exception):
    pass


@dataclass(frozen=True)
class CodexLabReleaseSummary:
    published_at: str
    tag_name: str


@dataclass(frozen=True)
class CodexLabInstallResult:
    app_dir: Path
    code_mode_host_path: Path | None
    engine_path: Path
    release_tag: str
    shim_path: Path | None
    state_path: Path
    supervisor_label: str
    version: str


@dataclass(frozen=True)
class CodexLabInstallStatus:
    app_path: Path
    bundle_version: str
    code_route: CodeRouteState | None
    code_mode_host_backup_path: Path | None
    code_mode_host_backup_sha256: str | None
    code_mode_host_path: Path | None
    code_mode_host_sha256: str | None
    code_mode_host_signing_identifier: str | None
    code_mode_host_team_identifier: str | None
    engine_backup_path: Path | None
    engine_backup_sha256: str | None
    engine_path: Path | None
    engine_sha256: str | None
    engine_signing_identifier: str | None
    engine_team_identifier: str | None
    lab_home: Path | None
    launch_agents_dir: Path | None
    listen_host: str | None
    listen_port: int | None
    release_tag: str
    release_version: str
    shim_path: Path | None
    source_commit: str | None
    source_repository: str | None
    state_path: Path
    supervisor_label: str | None
    version: str


@dataclass(frozen=True)
class CodexLabUpdateCheck:
    installed: CodexLabInstallStatus
    latest_release_tag: str
    update_available: bool


@dataclass(frozen=True)
class CodexLabUpdateResult:
    check: CodexLabUpdateCheck
    install: CodexLabInstallResult | None


@dataclass(frozen=True)
class CodexLabUninstallResult:
    app_path: Path
    code_mode_host_path: Path | None
    engine_path: Path | None
    restored_code_mode_host_path: Path | None
    restored_code_route_path: Path | None
    restored_engine_path: Path | None
    shim_path: Path | None
    state_path: Path


@dataclass(frozen=True)
class ManagedEngineRelease:
    code_mode_host: "ManagedCodeModeHostRelease | None"
    release_version: str
    sha256: str
    signing_identifier: str
    source_commit: str
    team_identifier: str
    version: str


@dataclass(frozen=True)
class ManagedCodeModeHostRelease:
    sha256: str
    signing_identifier: str
    team_identifier: str


InspectEngineFunc = Callable[[Path], EngineIdentity]
InspectCodeModeHostFunc = Callable[[Path], CodeModeHostIdentity]
InstallSupervisorFunc = Callable[[SupervisorPaths, ManagedEngineRelease], None]
UninstallSupervisorFunc = Callable[[SupervisorPaths], None]


@dataclass(frozen=True)
class EngineProvisioningOperations:
    inspect: InspectEngineFunc
    install_supervisor: InstallSupervisorFunc
    uninstall_supervisor: UninstallSupervisorFunc
    inspect_code_mode_host: InspectCodeModeHostFunc | None = None


@dataclass(frozen=True)
class Replacement:
    target: Path
    backup_path: Path
    preserve_backup: bool = False


def install_release_supervisor(
    paths: SupervisorPaths,
    release: ManagedEngineRelease,
) -> None:
    code_mode_host = release.code_mode_host
    install_supervisor(
        paths,
        expected_sha256=release.sha256,
        expected_source_commit=release.source_commit,
        expected_release_version=release.release_version,
        expected_version=release.version,
        expected_code_mode_host_sha256=code_mode_host.sha256
        if code_mode_host is not None
        else None,
        expected_code_mode_host_signing_identifier=code_mode_host.signing_identifier
        if code_mode_host is not None
        else None,
        expected_code_mode_host_team_identifier=code_mode_host.team_identifier
        if code_mode_host is not None
        else None,
    )


def uninstall_release_supervisor(paths: SupervisorPaths) -> None:
    uninstall_supervisor(paths)


DEFAULT_ENGINE_OPERATIONS = EngineProvisioningOperations(
    inspect=inspect_engine,
    install_supervisor=install_release_supervisor,
    uninstall_supervisor=uninstall_release_supervisor,
    inspect_code_mode_host=inspect_code_mode_host,
)


def manifest_url_for_release_tag(
    release_tag: str,
    *,
    repository: str = DEFAULT_REPOSITORY,
) -> str:
    return f"https://github.com/{repository}/releases/download/{release_tag}/{MANIFEST_NAME}"


def manifest_url_for_latest_release(
    *,
    repository: str = DEFAULT_REPOSITORY,
) -> str:
    return manifest_url_for_release_tag(
        latest_release_tag(repository=repository), repository=repository
    )


def latest_release_tag(*, repository: str = DEFAULT_REPOSITORY) -> str:
    return select_latest_lab_release_summary(
        lab_distribution_release_summaries(repository=repository)
    ).tag_name


def lab_distribution_release_summaries(
    *, repository: str = DEFAULT_REPOSITORY
) -> list[CodexLabReleaseSummary]:
    candidates: list[object] = []
    for page in range(1, MAX_LATEST_RELEASE_PAGES + 1):
        releases = download_json_url(github_releases_url(repository, page=page))
        if not isinstance(releases, list):
            raise ValueError("GitHub releases response must be a list")
        if not releases:
            break
        candidates.extend(releases)
    return [
        summary
        for release in candidates
        if (summary := lab_distribution_release_summary(release)) is not None
    ]


def install_from_manifest_url(
    manifest_url: str,
    *,
    app_dir: Path = DEFAULT_APP_DIR,
    shim_dir: Path | None = DEFAULT_SHIM_DIR,
    state_path: Path = DEFAULT_STATE_PATH,
    supervisor_paths: SupervisorPaths | None = None,
    force: bool = False,
    download: DownloadFunc | None = None,
    engine_operations: EngineProvisioningOperations | None = None,
) -> CodexLabInstallResult:
    state_path = resolve_destination(state_path)
    with transaction_lock(state_path):
        recover_code_route_transaction(state_path, lock_held=True)
        return _install_from_manifest_url_locked(
            manifest_url,
            app_dir=app_dir,
            shim_dir=shim_dir,
            state_path=state_path,
            supervisor_paths=supervisor_paths,
            force=force,
            download=download,
            engine_operations=engine_operations,
        )


def _install_from_manifest_url_locked(
    manifest_url: str,
    *,
    app_dir: Path = DEFAULT_APP_DIR,
    shim_dir: Path | None = DEFAULT_SHIM_DIR,
    state_path: Path = DEFAULT_STATE_PATH,
    supervisor_paths: SupervisorPaths | None = None,
    force: bool = False,
    download: DownloadFunc | None = None,
    engine_operations: EngineProvisioningOperations | None = None,
) -> CodexLabInstallResult:
    if not is_https_url(manifest_url):
        raise ValueError(f"manifest URL must be an HTTPS URL: {manifest_url}")
    download = download or download_url
    engine_operations = engine_operations or DEFAULT_ENGINE_OPERATIONS
    supervisor_paths = supervisor_paths or default_supervisor_paths()

    app_dir = resolve_destination(app_dir)
    shim_path = resolve_destination(shim_dir / "codex-lab") if shim_dir else None
    state_path = resolve_destination(state_path)
    previous_status = read_optional_install_state(state_path, lock_held=True)
    require_code_route_inactive_for_install(previous_status)

    with tempfile.TemporaryDirectory(prefix="codex-lab-install-") as temp_dir_name:
        temp_dir = Path(temp_dir_name)
        dist_dir = temp_dir / "dist"
        extract_dir = temp_dir / "extract"
        dist_dir.mkdir()
        extract_dir.mkdir()

        manifest_path = dist_dir / MANIFEST_NAME
        sha256sums_path = dist_dir / SHA256SUMS_NAME
        download(manifest_url, manifest_path)
        download(sibling_url(manifest_url, SHA256SUMS_NAME), sha256sums_path)

        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        validate_manifest(manifest)
        release = manifest.get("release")
        if not isinstance(release, dict):
            raise ValueError("Installer requires a published release manifest")
        engine_release = managed_engine_release_from_manifest(manifest)

        artifacts = manifest["artifacts"]
        for artifact in ARTIFACTS:
            entry = artifacts[artifact.role]
            require_sibling_download_url(
                manifest_url,
                entry["downloadUrl"],
                artifact.file_name,
            )
            download(entry["downloadUrl"], dist_dir / artifact.file_name)

        checksums = read_sha256sums(sha256sums_path)
        validate_manifest(manifest, dist_dir=dist_dir, checksums=checksums)

        legacy_release = None
        if previous_status is not None and install_state_lacks_exact_identity(
            previous_status
        ):
            legacy_release = download_recorded_release_identity(
                previous_status,
                download=download,
                destination=temp_dir / "recorded-release-manifest.json",
            )

        app_source = extract_artifact(
            dist_dir / APP_ZIP,
            archive_root="Codex Lab.app",
            extract_dir=extract_dir / "app",
        )
        shim_source = extract_artifact(
            dist_dir / SHIM_ZIP,
            archive_root="bin/codex-lab",
            extract_dir=extract_dir / "shim",
        )
        engine_archive_source = extract_artifact(
            dist_dir / ENGINE_ZIP,
            archive_root=artifacts["engineZip"]["archiveRoot"],
            extract_dir=extract_dir / "engine",
        )
        if engine_release.code_mode_host is None:
            engine_source = engine_archive_source
            code_mode_host_source = None
        else:
            engine_source = engine_archive_source / Path(ENGINE_CLI_ARCHIVE_PATH).name
            code_mode_host_source = (
                engine_archive_source / Path(CODE_MODE_HOST_ARCHIVE_PATH).name
            )
        smoke_check(app_source, shim_source)
        staged_identity = engine_operations.inspect(engine_source)
        require_engine_release_identity(staged_identity, engine_release)
        if engine_release.code_mode_host is not None:
            if engine_operations.inspect_code_mode_host is None:
                raise ValueError("Code Mode host inspection is unavailable")
            assert code_mode_host_source is not None
            staged_code_mode_host_identity = engine_operations.inspect_code_mode_host(
                code_mode_host_source
            )
            require_code_mode_host_release_identity(
                staged_code_mode_host_identity,
                engine_release.code_mode_host,
            )

        preflight_install_parent(app_dir.parent)
        preflight_install_target(app_dir, force=force)
        if shim_path is not None:
            preflight_install_parent(shim_path.parent)
            preflight_install_target(shim_path, force=force)
        preflight_install_parent(state_path.parent)
        preflight_install_parent(supervisor_paths.managed_cli.parent)
        preflight_install_target(supervisor_paths.managed_cli, force=force)
        if code_mode_host_source is not None:
            preflight_install_target(supervisor_paths.code_mode_host, force=force)

        if previous_status is not None:
            require_recorded_install(
                previous_status,
                supervisor_paths=supervisor_paths,
                engine_operations=engine_operations,
                legacy_release=legacy_release,
            )
        if (
            previous_status is not None
            and previous_status.engine_path is not None
            and previous_status.engine_path != supervisor_paths.managed_cli
        ):
            raise ValueError(
                "Recorded managed engine path does not match the requested Lab home: "
                f"{previous_status.engine_path} != {supervisor_paths.managed_cli}"
            )
        engine_backup_path = (
            previous_status.engine_backup_path if previous_status is not None else None
        )
        if engine_backup_path is not None:
            require_engine_backup(engine_backup_path)
        code_mode_host_backup_path = (
            previous_status.code_mode_host_backup_path
            if previous_status is not None
            else None
        )
        if code_mode_host_backup_path is not None:
            require_engine_backup(code_mode_host_backup_path)

        engine_was_installer_managed = (
            previous_status is not None
            and previous_status.engine_path == supervisor_paths.managed_cli
        )
        preserve_existing_engine = (
            not engine_was_installer_managed
            and engine_backup_path is None
            and path_exists(supervisor_paths.managed_cli)
        )
        if preserve_existing_engine:
            engine_backup_path = default_engine_backup_path(state_path)
            preflight_new_backup_path(engine_backup_path)

        code_mode_host_was_installer_managed = (
            previous_status is not None
            and previous_status.code_mode_host_path == supervisor_paths.code_mode_host
        )
        preserve_existing_code_mode_host = (
            code_mode_host_source is not None
            and not code_mode_host_was_installer_managed
            and code_mode_host_backup_path is None
            and path_exists(supervisor_paths.code_mode_host)
        )
        if preserve_existing_code_mode_host:
            code_mode_host_backup_path = default_code_mode_host_backup_path(state_path)
            preflight_new_backup_path(code_mode_host_backup_path)

        replacements = []
        installed_shim = None
        try:
            if code_mode_host_source is not None:
                replacements.append(
                    replace_path(
                        code_mode_host_source,
                        supervisor_paths.code_mode_host,
                        force=force,
                        backup_path=code_mode_host_backup_path
                        if preserve_existing_code_mode_host
                        else None,
                        preserve_backup=preserve_existing_code_mode_host,
                    )
                )
            elif code_mode_host_was_installer_managed and path_exists(
                supervisor_paths.code_mode_host
            ):
                replacements.append(stage_path_removal(supervisor_paths.code_mode_host))
            engine_replacement = replace_path(
                engine_source,
                supervisor_paths.managed_cli,
                force=force,
                backup_path=engine_backup_path if preserve_existing_engine else None,
                preserve_backup=preserve_existing_engine,
            )
            replacements.append(engine_replacement)
            app_replacement = replace_path(
                app_source,
                app_dir,
                force=force,
            )
            replacements.append(app_replacement)
            installed_app = app_replacement.target
            if shim_path is not None:
                shim_replacement = replace_path(
                    shim_source,
                    shim_path,
                    force=force,
                )
                replacements.append(shim_replacement)
                installed_shim = shim_replacement.target
                installed_shim.write_text(
                    build_shim_script(installed_app), encoding="utf-8"
                )
                make_executable(installed_shim)
            smoke_check(installed_app, installed_shim)
            state_source = temp_dir / "install-state.json"
            write_install_state(
                state_source,
                manifest,
                app_dir=installed_app,
                code_mode_host_backup_path=code_mode_host_backup_path,
                code_mode_host_path=supervisor_paths.code_mode_host
                if code_mode_host_source is not None
                else None,
                engine_backup_path=engine_backup_path,
                shim_path=installed_shim,
                supervisor_paths=supervisor_paths,
                code_route=None,
            )
            replacements.append(
                replace_path(
                    state_source,
                    state_path,
                    force=True,
                )
            )
            engine_operations.install_supervisor(supervisor_paths, engine_release)
        except Exception as install_error:
            try:
                rollback_replacements(replacements)
            except Exception as rollback_error:
                raise CodexLabRollbackError(
                    "Codex Lab installation failed and file rollback did not complete: "
                    f"{rollback_error}"
                ) from install_error
            raise
        cleanup_replacements(replacements)
        return CodexLabInstallResult(
            app_dir=installed_app,
            code_mode_host_path=supervisor_paths.code_mode_host
            if code_mode_host_source is not None
            else None,
            engine_path=supervisor_paths.managed_cli,
            release_tag=release["tag"],
            shim_path=installed_shim,
            state_path=state_path,
            supervisor_label=supervisor_paths.label,
            version=manifest["version"],
        )


def managed_engine_release_from_manifest(manifest: dict) -> ManagedEngineRelease:
    managed_engine = manifest["managedEngine"]
    companions = managed_engine.get("companions")
    code_mode_host = None
    if isinstance(companions, dict):
        host = companions["codeModeHost"]
        code_mode_host = ManagedCodeModeHostRelease(
            sha256=host["sha256"],
            signing_identifier=host["signingIdentifier"],
            team_identifier=host["teamIdentifier"],
        )
    return ManagedEngineRelease(
        code_mode_host=code_mode_host,
        release_version=manifest_release_version(manifest),
        sha256=managed_engine["sha256"],
        signing_identifier=managed_engine["signingIdentifier"],
        source_commit=managed_engine["sourceCommit"],
        team_identifier=managed_engine["teamIdentifier"],
        version=managed_engine["version"],
    )


def managed_engine_release_from_identity(
    identity: EngineIdentity,
    code_mode_host_identity: CodeModeHostIdentity | None = None,
) -> ManagedEngineRelease:
    return ManagedEngineRelease(
        code_mode_host=ManagedCodeModeHostRelease(
            sha256=code_mode_host_identity.sha256,
            signing_identifier=code_mode_host_identity.signing_identifier,
            team_identifier=code_mode_host_identity.team_identifier,
        )
        if code_mode_host_identity is not None
        else None,
        release_version=identity.release_version,
        sha256=identity.sha256,
        signing_identifier=identity.signing_identifier,
        source_commit=identity.source_commit,
        team_identifier=identity.team_identifier,
        version=identity.version,
    )


def require_engine_release_identity(
    identity: EngineIdentity,
    release: ManagedEngineRelease,
) -> None:
    expected = {
        "release version": release.release_version,
        "sha256": release.sha256,
        "signing identifier": release.signing_identifier,
        "source commit": release.source_commit,
        "team identifier": release.team_identifier,
        "version": release.version,
    }
    actual = {
        "release version": identity.release_version,
        "sha256": identity.sha256,
        "signing identifier": identity.signing_identifier,
        "source commit": identity.source_commit,
        "team identifier": identity.team_identifier,
        "version": identity.version,
    }
    mismatches = [
        f"{field}: {actual[field]} != {expected[field]}"
        for field in expected
        if actual[field] != expected[field]
    ]
    if mismatches:
        raise ValueError(
            "Managed engine identity does not match the release manifest: "
            + "; ".join(mismatches)
        )


def require_code_mode_host_release_identity(
    identity: CodeModeHostIdentity,
    release: ManagedCodeModeHostRelease,
) -> None:
    actual = (
        identity.sha256,
        identity.signing_identifier,
        identity.team_identifier,
    )
    expected = (
        release.sha256,
        release.signing_identifier,
        release.team_identifier,
    )
    if actual != expected:
        raise ValueError(
            "Managed Code Mode host identity does not match the release manifest"
        )


def install_state_lacks_exact_identity(status: CodexLabInstallStatus) -> bool:
    engine_identity = (
        status.engine_sha256,
        status.engine_signing_identifier,
        status.engine_team_identifier,
    )
    host_identity = (
        status.code_mode_host_sha256,
        status.code_mode_host_signing_identifier,
        status.code_mode_host_team_identifier,
    )
    return (
        status.engine_path is not None
        and any(value is None for value in engine_identity)
    ) or (
        status.code_mode_host_path is not None
        and any(value is None for value in host_identity)
    )


def download_recorded_release_identity(
    status: CodexLabInstallStatus,
    *,
    download: DownloadFunc,
    destination: Path,
) -> ManagedEngineRelease:
    if status.source_repository is None or status.source_commit is None:
        raise ValueError(
            "Install state lacks the published source identity required to verify "
            "this legacy installation"
        )
    manifest_url = manifest_url_for_release_tag(
        status.release_tag,
        repository=status.source_repository,
    )
    download(manifest_url, destination)
    manifest = json.loads(destination.read_text(encoding="utf-8"))
    validate_manifest(manifest)
    release = manifest.get("release")
    source = manifest.get("source")
    if (
        not isinstance(release, dict)
        or release.get("tag") != status.release_tag
        or manifest.get("version") != status.version
        or manifest_release_version(manifest) != status.release_version
        or not isinstance(source, dict)
        or source.get("commit") != status.source_commit
        or source.get("repository") != status.source_repository
    ):
        raise ValueError(
            "Published release identity does not match the legacy install state"
        )
    return managed_engine_release_from_manifest(manifest)


def read_optional_install_state(
    state_path: Path,
    *,
    lock_held: bool = False,
) -> CodexLabInstallStatus | None:
    if not lock_held:
        with transaction_lock(state_path):
            return read_optional_install_state(state_path, lock_held=True)
    recover_code_route_transaction(state_path, lock_held=True)
    if not path_exists(state_path):
        return None
    return read_install_state(state_path, lock_held=True)


def require_code_route_inactive_for_install(
    status: CodexLabInstallStatus | None,
) -> None:
    if status is not None and status.code_route is not None:
        raise ValueError(
            "Deactivate the explicit code route before installing or updating Codex Lab"
        )


def require_recorded_install(
    status: CodexLabInstallStatus,
    *,
    supervisor_paths: SupervisorPaths,
    engine_operations: EngineProvisioningOperations,
    legacy_release: ManagedEngineRelease | None = None,
) -> None:
    try:
        smoke_check(status.app_path, status.shim_path)
    except (OSError, subprocess.CalledProcessError, ValueError) as exc:
        raise ValueError(
            "Recorded app or shim is not a managed Codex Lab install"
        ) from exc
    require_recorded_backups(status)
    if status.engine_path is None:
        return
    if status.engine_path != supervisor_paths.managed_cli:
        raise ValueError(
            "Recorded managed engine path does not match the requested Lab home: "
            f"{status.engine_path} != {supervisor_paths.managed_cli}"
        )
    identity = engine_operations.inspect(status.engine_path)
    expected_signed_identity = (
        status.engine_sha256
        if status.engine_sha256 is not None
        else legacy_release.sha256
        if legacy_release is not None
        else None,
        status.engine_signing_identifier
        if status.engine_signing_identifier is not None
        else legacy_release.signing_identifier
        if legacy_release is not None
        else None,
        status.engine_team_identifier
        if status.engine_team_identifier is not None
        else legacy_release.team_identifier
        if legacy_release is not None
        else None,
    )
    actual_signed_identity = (
        identity.sha256,
        identity.signing_identifier,
        identity.team_identifier,
    )
    provenance_mismatch = (
        identity.release_version != status.release_version
        or identity.version != status.version
        or (
            status.source_commit is not None
            and identity.source_commit != status.source_commit
        )
    )
    signed_identity_mismatch = any(
        value is not None for value in expected_signed_identity
    ) and (
        any(value is None for value in expected_signed_identity)
        or actual_signed_identity != expected_signed_identity
    )
    if provenance_mismatch or signed_identity_mismatch:
        raise ValueError(
            "Recorded managed engine provenance does not match the install state"
        )
    if status.code_mode_host_path is not None:
        if status.code_mode_host_path != supervisor_paths.code_mode_host:
            raise ValueError(
                "Recorded Code Mode host path does not match the requested Lab home"
            )
        if engine_operations.inspect_code_mode_host is None:
            raise ValueError("Code Mode host inspection is unavailable")
        identity = engine_operations.inspect_code_mode_host(status.code_mode_host_path)
        legacy_host = (
            legacy_release.code_mode_host if legacy_release is not None else None
        )
        expected_identity = (
            status.code_mode_host_sha256
            if status.code_mode_host_sha256 is not None
            else legacy_host.sha256
            if legacy_host is not None
            else None,
            status.code_mode_host_signing_identifier
            if status.code_mode_host_signing_identifier is not None
            else legacy_host.signing_identifier
            if legacy_host is not None
            else None,
            status.code_mode_host_team_identifier
            if status.code_mode_host_team_identifier is not None
            else legacy_host.team_identifier
            if legacy_host is not None
            else None,
        )
        if any(value is None for value in expected_identity):
            raise ValueError(
                "Install state lacks the exact Code Mode host identity; reinstall "
                "the recorded release before updating or uninstalling"
            )
        if (
            identity.sha256,
            identity.signing_identifier,
            identity.team_identifier,
        ) != expected_identity:
            raise ValueError(
                "Recorded Code Mode host identity does not match the install state"
            )


def default_engine_backup_path(state_path: Path) -> Path:
    return state_path.parent / "engine-backup" / "codex"


def default_code_mode_host_backup_path(state_path: Path) -> Path:
    return state_path.parent / "engine-backup" / "codex-code-mode-host"


def require_recorded_backups(status: CodexLabInstallStatus) -> None:
    recorded_backups = (
        (
            status.engine_backup_path,
            default_engine_backup_path(status.state_path),
            status.engine_backup_sha256,
            "engine",
        ),
        (
            status.code_mode_host_backup_path,
            default_code_mode_host_backup_path(status.state_path),
            status.code_mode_host_backup_sha256,
            "Code Mode host",
        ),
    )
    for path, expected_path, expected_sha256, description in recorded_backups:
        if path is None:
            if expected_sha256 is not None:
                raise ValueError(
                    f"Recorded {description} backup digest has no backup path"
                )
            continue
        if path != expected_path:
            raise ValueError(f"Recorded {description} backup path is unsafe: {path}")
        require_engine_backup(path, expected_sha256=expected_sha256)


def require_engine_backup(path: Path, *, expected_sha256: str | None = None) -> None:
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"Recorded engine backup is not a regular file: {path}")
    if expected_sha256 is not None and sha256_file(path) != expected_sha256:
        raise ValueError(f"Recorded engine backup digest has changed: {path}")


def preflight_new_backup_path(path: Path) -> None:
    preflight_install_parent(path.parent)
    if path_exists(path):
        raise FileExistsError(f"Engine backup path already exists: {path}")


def read_install_state(
    state_path: Path = DEFAULT_STATE_PATH,
    *,
    lock_held: bool = False,
) -> CodexLabInstallStatus:
    state_path = resolve_destination(state_path)
    if not lock_held:
        with transaction_lock(state_path):
            return read_install_state(state_path, lock_held=True)
    recover_code_route_transaction(state_path, lock_held=True)
    try:
        state = json.loads(state_path.read_text(encoding="utf-8"))
    except FileNotFoundError as exc:
        raise CodexLabInstallStateError(
            f"Codex Lab install state not found: {state_path}"
        ) from exc
    except (OSError, json.JSONDecodeError) as exc:
        raise CodexLabInstallStateError(
            f"Could not read Codex Lab install state: {exc}"
        ) from exc
    if not isinstance(state, dict):
        raise CodexLabInstallStateError(
            f"Install state must be a JSON object: {state_path}"
        )

    source = state.get("source")
    source_commit = None
    source_repository = None
    if isinstance(source, dict):
        commit = source.get("commit")
        if isinstance(commit, str):
            source_commit = commit
        repository = source.get("repository")
        if isinstance(repository, str):
            source_repository = repository

    managed_engine = state.get("managedEngine")
    if managed_engine is not None and not isinstance(managed_engine, dict):
        raise CodexLabInstallStateError(
            f"Install state field managedEngine must be an object or null: {state_path}"
        )
    engine_sha256 = None
    engine_signing_identifier = None
    engine_team_identifier = None
    if isinstance(managed_engine, dict):
        engine_sha256 = optional_state_sha256(managed_engine, "sha256", state_path)
        engine_signing_identifier = optional_state_string(
            managed_engine, "signingIdentifier", state_path
        )
        engine_team_identifier = optional_state_string(
            managed_engine, "teamIdentifier", state_path
        )

    managed_code_mode_host = state.get("managedCodeModeHost")
    if managed_code_mode_host is not None and not isinstance(
        managed_code_mode_host, dict
    ):
        raise CodexLabInstallStateError(
            "Install state field managedCodeModeHost must be an object or null: "
            f"{state_path}"
        )
    code_mode_host_sha256 = None
    code_mode_host_signing_identifier = None
    code_mode_host_team_identifier = None
    if isinstance(managed_code_mode_host, dict):
        code_mode_host_sha256 = optional_state_sha256(
            managed_code_mode_host, "sha256", state_path
        )
        code_mode_host_signing_identifier = optional_state_string(
            managed_code_mode_host, "signingIdentifier", state_path
        )
        code_mode_host_team_identifier = optional_state_string(
            managed_code_mode_host, "teamIdentifier", state_path
        )

    shim_path = state.get("shimPath")
    if shim_path is not None and not isinstance(shim_path, str):
        raise CodexLabInstallStateError(
            f"Install state field shimPath must be a string or null: {state_path}"
        )
    engine_backup_path = optional_state_path(state, "engineBackupPath", state_path)
    engine_backup_sha256 = optional_state_sha256(
        state, "engineBackupSha256", state_path
    )
    engine_path = optional_state_path(state, "enginePath", state_path)
    code_mode_host_backup_path = optional_state_path(
        state, "codeModeHostBackupPath", state_path
    )
    code_mode_host_backup_sha256 = optional_state_sha256(
        state, "codeModeHostBackupSha256", state_path
    )
    code_mode_host_path = optional_state_path(state, "codeModeHostPath", state_path)
    lab_home = optional_state_path(state, "labHome", state_path)
    launch_agents_dir = optional_state_path(state, "launchAgentsDir", state_path)
    listen_host = optional_state_string(state, "listenHost", state_path)
    listen_port = optional_state_positive_int(state, "listenPort", state_path)
    supervisor_label = state.get("supervisorLabel")
    if supervisor_label is not None and (
        not isinstance(supervisor_label, str) or not supervisor_label
    ):
        raise CodexLabInstallStateError(
            f"Install state field supervisorLabel must be a non-empty string or null: {state_path}"
        )
    try:
        code_route = read_code_route_state(state, state_path)
    except ValueError as exc:
        raise CodexLabInstallStateError(str(exc)) from exc
    release_tag = required_state_string(state, "releaseTag", state_path)
    release_version = optional_state_string(state, "releaseVersion", state_path)
    if release_version is None:
        try:
            release_version = release_identity_from_tag(release_tag)
        except ValueError as exc:
            raise CodexLabInstallStateError(str(exc)) from exc
    return CodexLabInstallStatus(
        app_path=Path(required_state_string(state, "appPath", state_path)),
        bundle_version=required_state_string(state, "bundleVersion", state_path),
        code_route=code_route,
        code_mode_host_backup_path=code_mode_host_backup_path,
        code_mode_host_backup_sha256=code_mode_host_backup_sha256,
        code_mode_host_path=code_mode_host_path,
        code_mode_host_sha256=code_mode_host_sha256,
        code_mode_host_signing_identifier=code_mode_host_signing_identifier,
        code_mode_host_team_identifier=code_mode_host_team_identifier,
        engine_backup_path=engine_backup_path,
        engine_backup_sha256=engine_backup_sha256,
        engine_path=engine_path,
        engine_sha256=engine_sha256,
        engine_signing_identifier=engine_signing_identifier,
        engine_team_identifier=engine_team_identifier,
        lab_home=lab_home,
        launch_agents_dir=launch_agents_dir,
        listen_host=listen_host,
        listen_port=listen_port,
        release_tag=release_tag,
        release_version=release_version,
        shim_path=Path(shim_path) if isinstance(shim_path, str) else None,
        source_commit=source_commit,
        source_repository=source_repository,
        state_path=state_path,
        supervisor_label=supervisor_label,
        version=required_state_string(state, "version", state_path),
    )


def activate_code_route(
    *,
    state_path: Path = DEFAULT_STATE_PATH,
    code_route_path: Path = DEFAULT_CODE_ROUTE_PATH,
    engine_operations: EngineProvisioningOperations | None = None,
) -> CodeRouteResult:
    state_path = resolve_destination(state_path)
    with transaction_lock(state_path):
        return _activate_code_route_locked(
            state_path=state_path,
            code_route_path=code_route_path,
            engine_operations=engine_operations,
        )


def _activate_code_route_locked(
    *,
    state_path: Path,
    code_route_path: Path,
    engine_operations: EngineProvisioningOperations | None,
) -> CodeRouteResult:
    status = read_install_state(state_path, lock_held=True)
    engine_operations = engine_operations or DEFAULT_ENGINE_OPERATIONS
    expected_paths = default_supervisor_paths()
    if status.lab_home != expected_paths.lab_home:
        raise ValueError(
            "The code route requires the default Codex Lab home: "
            f"{expected_paths.lab_home}"
        )
    if status.engine_path != expected_paths.managed_cli:
        raise ValueError(
            "The code route requires the default managed engine path: "
            f"{expected_paths.managed_cli}"
        )
    assert status.engine_path is not None
    assert status.lab_home is not None
    if (
        status.engine_sha256 is None
        or status.engine_signing_identifier is None
        or status.engine_team_identifier is None
        or status.source_commit is None
    ):
        raise ValueError(
            "Installed Codex Lab state lacks the managed release identity required "
            "for code route activation; install a current published release first"
        )
    identity = engine_operations.inspect(status.engine_path)
    recorded_identity = (
        status.engine_sha256,
        status.engine_signing_identifier,
        status.source_commit,
        status.engine_team_identifier,
        status.release_version,
        status.version,
    )
    actual_identity = (
        identity.sha256,
        identity.signing_identifier,
        identity.source_commit,
        identity.team_identifier,
        identity.release_version,
        identity.version,
    )
    if actual_identity != recorded_identity:
        raise ValueError(
            "Installed managed engine identity does not match the installer state"
        )
    return activate_installed_code_route(
        status.state_path,
        CodeRouteEngine(
            path=status.engine_path,
            sha256=identity.sha256,
            signing_identifier=identity.signing_identifier,
            source_commit=identity.source_commit,
            team_identifier=identity.team_identifier,
            release_tag=status.release_tag,
            release_version=status.release_version,
            version=identity.version,
            build_channel=identity.build_channel,
            lab_home=status.lab_home,
        ),
        active_path=code_route_path,
        lock_held=True,
    )


def deactivate_code_route(
    *,
    state_path: Path = DEFAULT_STATE_PATH,
    code_route_path: Path = DEFAULT_CODE_ROUTE_PATH,
) -> CodeRouteResult:
    state_path = resolve_destination(state_path)
    with transaction_lock(state_path):
        status = read_install_state(state_path, lock_held=True)
        if status.code_route is not None:
            require_active_code_route(status.code_route, expected_path=code_route_path)
        return deactivate_installed_code_route(
            status.state_path,
            active_path=code_route_path,
            lock_held=True,
        )


def require_recorded_code_route(status: CodexLabInstallStatus) -> None:
    if status.code_route is not None:
        require_active_code_route(
            status.code_route,
            expected_path=status.code_route.active_path,
        )


def require_recorded_install_status(
    status: CodexLabInstallStatus,
    *,
    engine_operations: EngineProvisioningOperations | None = None,
) -> None:
    require_recorded_install(
        status,
        supervisor_paths=supervisor_paths_from_status(status),
        engine_operations=engine_operations or DEFAULT_ENGINE_OPERATIONS,
    )
    require_recorded_code_route(status)


def read_verified_install_status(
    state_path: Path = DEFAULT_STATE_PATH,
    *,
    engine_operations: EngineProvisioningOperations | None = None,
) -> CodexLabInstallStatus:
    state_path = resolve_destination(state_path)
    with transaction_lock(state_path):
        status = read_install_state(state_path, lock_held=True)
        require_recorded_install_status(
            status,
            engine_operations=engine_operations,
        )
        return status


def check_for_update(
    *,
    repository: str = DEFAULT_REPOSITORY,
    state_path: Path = DEFAULT_STATE_PATH,
    lock_held: bool = False,
    validate_install: bool = True,
) -> CodexLabUpdateCheck:
    state_path = resolve_destination(state_path)
    if not lock_held:
        with transaction_lock(state_path):
            return check_for_update(
                repository=repository,
                state_path=state_path,
                lock_held=True,
                validate_install=validate_install,
            )
    installed = read_install_state(state_path, lock_held=lock_held)
    if validate_install:
        require_recorded_install_status(installed)
    releases = lab_distribution_release_summaries(repository=repository)
    if not any(release.tag_name == installed.release_tag for release in releases):
        raise CodexLabUpdateError(
            f"Installed Codex Lab release is not in the published Lab release list: {installed.release_tag}"
        )
    latest = select_latest_ordered_lab_release_summary(releases)
    installed_order = codex_lab_release_order(installed.release_tag)
    if installed_order is None:
        raise CodexLabUpdateError(
            f"Installed Codex Lab release tag cannot be ordered safely: {installed.release_tag}"
        )
    latest_order = codex_lab_release_order(latest.tag_name)
    assert latest_order is not None
    return CodexLabUpdateCheck(
        installed=installed,
        latest_release_tag=latest.tag_name,
        update_available=latest_order > installed_order,
    )


def update_from_latest_release(
    *,
    repository: str = DEFAULT_REPOSITORY,
    state_path: Path = DEFAULT_STATE_PATH,
    download: DownloadFunc | None = None,
    engine_operations: EngineProvisioningOperations | None = None,
) -> CodexLabUpdateResult:
    state_path = resolve_destination(state_path)
    with transaction_lock(state_path):
        return _update_from_latest_release_locked(
            repository=repository,
            state_path=state_path,
            download=download,
            engine_operations=engine_operations,
        )


def _update_from_latest_release_locked(
    *,
    repository: str,
    state_path: Path,
    download: DownloadFunc | None,
    engine_operations: EngineProvisioningOperations | None,
) -> CodexLabUpdateResult:
    require_code_route_inactive_for_install(
        read_install_state(state_path, lock_held=True)
    )
    check = check_for_update(
        repository=repository,
        state_path=state_path,
        lock_held=True,
        validate_install=False,
    )
    if not check.update_available:
        return CodexLabUpdateResult(check=check, install=None)

    manifest_url = manifest_url_for_release_tag(
        check.latest_release_tag,
        repository=repository,
    )
    installed = check.installed
    supervisor_paths = supervisor_paths_from_status(installed)
    return CodexLabUpdateResult(
        check=check,
        install=_install_from_manifest_url_locked(
            manifest_url,
            app_dir=installed.app_path,
            shim_dir=installed.shim_path.parent
            if installed.shim_path is not None
            else None,
            state_path=installed.state_path,
            supervisor_paths=supervisor_paths,
            force=True,
            download=download,
            engine_operations=engine_operations,
        ),
    )


def uninstall_codex_lab(
    *,
    state_path: Path = DEFAULT_STATE_PATH,
    engine_operations: EngineProvisioningOperations | None = None,
) -> CodexLabUninstallResult:
    state_path = resolve_destination(state_path)
    with transaction_lock(state_path):
        return _uninstall_codex_lab_locked(
            state_path=state_path,
            engine_operations=engine_operations,
        )


def _uninstall_codex_lab_locked(
    *,
    state_path: Path,
    engine_operations: EngineProvisioningOperations | None,
) -> CodexLabUninstallResult:
    engine_operations = engine_operations or DEFAULT_ENGINE_OPERATIONS
    status = read_install_state(state_path, lock_held=True)
    if status.code_route is not None:
        require_active_code_route(
            status.code_route,
            expected_path=status.code_route.active_path,
        )
        raise ValueError(
            "Deactivate the explicit code route before uninstalling Codex Lab"
        )
    supervisor_paths = supervisor_paths_from_status(status)
    require_recorded_install(
        status,
        supervisor_paths=supervisor_paths,
        engine_operations=engine_operations,
    )
    manages_engine = (
        status.engine_path is not None
        and status.supervisor_label is not None
        and status.engine_path == supervisor_paths.managed_cli
    )
    if status.engine_backup_path is not None:
        require_engine_backup(status.engine_backup_path)
    manages_code_mode_host = (
        manages_engine and status.code_mode_host_path == supervisor_paths.code_mode_host
    )
    if status.code_mode_host_backup_path is not None:
        require_engine_backup(status.code_mode_host_backup_path)
        if not manages_code_mode_host and path_exists(supervisor_paths.code_mode_host):
            raise ValueError(
                "Cannot restore the recorded Code Mode host backup over an unmanaged "
                f"path. Move {supervisor_paths.code_mode_host} aside before uninstalling "
                f"so {status.code_mode_host_backup_path} can be restored."
            )
    current_engine_release = None
    if manages_engine:
        current_code_mode_host_identity = None
        if manages_code_mode_host:
            if engine_operations.inspect_code_mode_host is None:
                raise ValueError("Code Mode host inspection is unavailable")
            current_code_mode_host_identity = engine_operations.inspect_code_mode_host(
                supervisor_paths.code_mode_host
            )
        current_engine_release = managed_engine_release_from_identity(
            engine_operations.inspect(supervisor_paths.managed_cli),
            current_code_mode_host_identity,
        )

    removals: list[Replacement] = []
    restored_engine_path = None
    restored_code_mode_host_path = None
    try:
        for target in (status.app_path, status.shim_path):
            if target is not None and path_exists(target):
                removals.append(stage_path_removal(target))
        removals.append(stage_path_removal(status.state_path))

        if manages_engine:
            engine_operations.uninstall_supervisor(supervisor_paths)
            if manages_code_mode_host:
                removals.append(stage_path_removal(supervisor_paths.code_mode_host))
            removals.append(stage_path_removal(supervisor_paths.managed_cli))
        if manages_engine and status.engine_backup_path is not None:
            supervisor_paths.managed_cli.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(
                str(status.engine_backup_path),
                str(supervisor_paths.managed_cli),
            )
            restored_engine_path = supervisor_paths.managed_cli
        if manages_engine and status.code_mode_host_backup_path is not None:
            supervisor_paths.code_mode_host.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(
                str(status.code_mode_host_backup_path),
                str(supervisor_paths.code_mode_host),
            )
            restored_code_mode_host_path = supervisor_paths.code_mode_host
    except Exception as uninstall_error:
        try:
            if restored_code_mode_host_path is not None:
                assert status.code_mode_host_backup_path is not None
                status.code_mode_host_backup_path.parent.mkdir(
                    parents=True, exist_ok=True
                )
                shutil.move(
                    str(restored_code_mode_host_path),
                    str(status.code_mode_host_backup_path),
                )
            if restored_engine_path is not None:
                assert status.engine_backup_path is not None
                status.engine_backup_path.parent.mkdir(parents=True, exist_ok=True)
                shutil.move(
                    str(restored_engine_path),
                    str(status.engine_backup_path),
                )
            rollback_replacements(removals)
            if current_engine_release is not None:
                engine_operations.install_supervisor(
                    supervisor_paths,
                    current_engine_release,
                )
        except Exception as rollback_error:
            raise CodexLabRollbackError(
                "Codex Lab uninstall failed and rollback did not complete: "
                f"{rollback_error}"
            ) from uninstall_error
        raise

    cleanup_replacements_best_effort(removals)
    return CodexLabUninstallResult(
        app_path=status.app_path,
        code_mode_host_path=status.code_mode_host_path
        if manages_code_mode_host
        else None,
        engine_path=status.engine_path if manages_engine else None,
        restored_code_mode_host_path=restored_code_mode_host_path,
        restored_code_route_path=None,
        restored_engine_path=restored_engine_path,
        shim_path=status.shim_path,
        state_path=status.state_path,
    )


def supervisor_paths_from_status(status: CodexLabInstallStatus) -> SupervisorPaths:
    if (
        status.lab_home is None
        or status.launch_agents_dir is None
        or status.listen_host is None
        or status.listen_port is None
        or status.supervisor_label is None
    ):
        return default_supervisor_paths()
    return SupervisorPaths(
        lab_home=status.lab_home,
        launch_agents_dir=status.launch_agents_dir,
        label=status.supervisor_label,
        listen_host=status.listen_host,
        listen_port=status.listen_port,
    )


def required_state_string(state: dict, field: str, state_path: Path) -> str:
    value = state.get(field)
    if not isinstance(value, str) or not value:
        raise CodexLabInstallStateError(
            f"Install state field {field} must be a non-empty string: {state_path}"
        )
    return value


def optional_state_path(state: dict, field: str, state_path: Path) -> Path | None:
    value = state.get(field)
    if value is None:
        return None
    if not isinstance(value, str) or not value:
        raise CodexLabInstallStateError(
            f"Install state field {field} must be a non-empty string or null: {state_path}"
        )
    return Path(value)


def optional_state_string(state: dict, field: str, state_path: Path) -> str | None:
    value = state.get(field)
    if value is None:
        return None
    if not isinstance(value, str) or not value:
        raise CodexLabInstallStateError(
            f"Install state field {field} must be a non-empty string or null: {state_path}"
        )
    return value


def optional_state_sha256(state: dict, field: str, state_path: Path) -> str | None:
    value = optional_state_string(state, field, state_path)
    if value is not None and (
        len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
    ):
        raise CodexLabInstallStateError(
            f"Install state field {field} must be a lowercase SHA-256 or null: {state_path}"
        )
    return value


def optional_state_positive_int(
    state: dict,
    field: str,
    state_path: Path,
) -> int | None:
    value = state.get(field)
    if value is None:
        return None
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise CodexLabInstallStateError(
            f"Install state field {field} must be a positive integer or null: {state_path}"
        )
    return value


def download_url(url: str, dest: Path) -> None:
    if not is_https_url(url):
        raise ValueError(f"download URL must be an HTTPS URL: {url}")
    dest.parent.mkdir(parents=True, exist_ok=True)
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with (
        urllib.request.urlopen(request, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response,
        dest.open("wb") as output,
    ):
        shutil.copyfileobj(response, output)


def download_json_url(url: str) -> object:
    if not is_https_url(url):
        raise ValueError(f"download URL must be an HTTPS URL: {url}")
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "User-Agent": USER_AGENT,
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(request, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response:
        return json.load(response)


def github_releases_url(repository: str, *, page: int = 1) -> str:
    parts = repository.split("/")
    if len(parts) != 2 or not all(parts):
        raise ValueError(f"GitHub repository must be OWNER/REPO: {repository}")
    if page < 1:
        raise ValueError(f"GitHub releases page must be positive: {page}")
    owner, repo = (urllib.parse.quote(part, safe="") for part in parts)
    return (
        f"https://api.github.com/repos/{owner}/{repo}/releases?per_page=100&page={page}"
    )


def select_latest_lab_release_tag(releases: object) -> str:
    if not isinstance(releases, list):
        raise ValueError("GitHub releases response must be a list")
    candidates = [
        release for release in releases if is_lab_distribution_release(release)
    ]
    if not candidates:
        raise ValueError(
            "No published Codex Lab release with a distribution manifest found"
        )
    latest = max(
        candidates,
        key=lambda release: (release.get("published_at") or "", release["tag_name"]),
    )
    return latest["tag_name"]


def select_latest_lab_release_summary(
    releases: list[CodexLabReleaseSummary],
) -> CodexLabReleaseSummary:
    if not releases:
        raise ValueError(
            "No published Codex Lab release with a distribution manifest found"
        )
    return max(releases, key=lambda release: (release.published_at, release.tag_name))


def select_latest_ordered_lab_release_summary(
    releases: list[CodexLabReleaseSummary],
) -> CodexLabReleaseSummary:
    ordered = [
        (release_order, release)
        for release in releases
        if (release_order := codex_lab_release_order(release.tag_name)) is not None
    ]
    if not ordered:
        raise CodexLabUpdateError(
            "No published Codex Lab release tag can be ordered safely"
        )
    _, release = max(ordered, key=lambda item: (item[0], item[1].published_at))
    return release


def lab_distribution_release_summary(
    release: object,
) -> CodexLabReleaseSummary | None:
    if not is_lab_distribution_release(release):
        return None
    assert isinstance(release, dict)
    tag_name = release["tag_name"]
    published_at = release.get("published_at") or ""
    if not isinstance(published_at, str):
        published_at = ""
    return CodexLabReleaseSummary(published_at=published_at, tag_name=tag_name)


def is_lab_distribution_release(release: object) -> bool:
    if not isinstance(release, dict):
        return False
    tag_name = release.get("tag_name")
    assets = release.get("assets")
    return (
        isinstance(tag_name, str)
        and tag_name.startswith(LAB_RELEASE_TAG_PREFIX)
        and not release.get("draft", False)
        and isinstance(assets, list)
        and any(
            isinstance(asset, dict)
            and asset.get("name") == MANIFEST_NAME
            and asset.get("state") == "uploaded"
            for asset in assets
        )
    )


def sibling_url(url: str, file_name: str) -> str:
    prefix, separator, _ = url.rpartition("/")
    if not separator:
        raise ValueError(f"URL has no parent path: {url}")
    return f"{prefix}/{file_name}"


def absolute_path(path: Path) -> Path:
    path = path.expanduser()
    if path.is_absolute():
        return path
    return Path.cwd() / path


def resolve_destination(path: Path) -> Path:
    path = absolute_path(path)
    return path.parent.resolve(strict=False) / path.name


def require_sibling_download_url(
    manifest_url: str,
    artifact_url: str,
    file_name: str,
) -> None:
    expected_url = sibling_url(manifest_url, file_name)
    if artifact_url != expected_url:
        raise ValueError(
            f"artifact URL does not match manifest release: {artifact_url}"
        )


def extract_artifact(zip_path: Path, *, archive_root: str, extract_dir: Path) -> Path:
    extract_dir.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(zip_path) as archive:
        for info in archive.infolist():
            validate_zip_member(info, archive_root=archive_root)
        for info in archive.infolist():
            extract_zip_member(archive, info, extract_dir)

    root = extract_dir / archive_root
    if not root.exists():
        raise FileNotFoundError(f"Archive {zip_path} did not contain {archive_root}")
    return root


def validate_zip_member(info: zipfile.ZipInfo, *, archive_root: str) -> None:
    name = info.filename
    path = Path(name)
    if path.is_absolute() or ".." in path.parts:
        raise ValueError(f"Unsafe zip member path: {name}")
    if any(part == "__MACOSX" or part.startswith("._") for part in path.parts):
        raise ValueError(f"Unexpected macOS metadata zip member: {name}")
    normalized_name = name.rstrip("/")
    if not (
        normalized_name == archive_root
        or normalized_name.startswith(f"{archive_root}/")
        or (info.is_dir() and archive_root.startswith(f"{normalized_name}/"))
    ):
        raise ValueError(f"Unexpected zip member outside {archive_root}: {name}")
    mode = (info.external_attr >> 16) & 0o777777
    file_type = stat.S_IFMT(mode)
    if file_type and not (stat.S_ISDIR(mode) or stat.S_ISREG(mode)):
        raise ValueError(f"Unsupported zip member file type: {name}")


def extract_zip_member(
    archive: zipfile.ZipFile,
    info: zipfile.ZipInfo,
    extract_dir: Path,
) -> None:
    target = extract_dir / info.filename
    if info.is_dir():
        target.mkdir(parents=True, exist_ok=True)
        return
    target.parent.mkdir(parents=True, exist_ok=True)
    with archive.open(info) as source, target.open("wb") as output:
        shutil.copyfileobj(source, output)
    mode = (info.external_attr >> 16) & 0o777
    if mode:
        target.chmod(mode)


def replace_path(
    source: Path,
    target: Path,
    *,
    force: bool,
    backup_path: Path | None = None,
    preserve_backup: bool = False,
) -> Replacement:
    target.parent.mkdir(parents=True, exist_ok=True)
    backup_path = backup_path or backup_path_for(target)
    if path_exists(target):
        preflight_install_target(target, force=force)
        backup_path.parent.mkdir(parents=True, exist_ok=True)
        if path_exists(backup_path):
            raise FileExistsError(f"Backup path already exists: {backup_path}")
        target.rename(backup_path)

    try:
        shutil.move(str(source), str(target))
    except Exception:
        remove_path(target)
        if path_exists(backup_path):
            shutil.move(str(backup_path), str(target))
        raise
    return Replacement(
        target=target,
        backup_path=backup_path,
        preserve_backup=preserve_backup,
    )


def stage_path_removal(target: Path) -> Replacement:
    backup_path = backup_path_for(target)
    target.rename(backup_path)
    return Replacement(target=target, backup_path=backup_path)


def backup_path_for(target: Path) -> Path:
    for _ in range(100):
        candidate = (
            target.parent / f".{target.name}.codex-lab-backup-{uuid.uuid4().hex}"
        )
        if not candidate.exists() and not candidate.is_symlink():
            return candidate
    raise FileExistsError(f"Could not allocate backup path for {target}")


def rollback_replacements(replacements: list[Replacement]) -> None:
    for replacement in reversed(replacements):
        remove_path(replacement.target)
        if path_exists(replacement.backup_path):
            shutil.move(str(replacement.backup_path), str(replacement.target))


def cleanup_replacements(replacements: list[Replacement]) -> None:
    for replacement in replacements:
        if not replacement.preserve_backup:
            remove_path(replacement.backup_path)


def cleanup_replacements_best_effort(replacements: list[Replacement]) -> None:
    for replacement in replacements:
        if replacement.preserve_backup:
            continue
        try:
            remove_path(replacement.backup_path)
        except OSError:
            continue


def remove_path(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    elif path.exists() or path.is_symlink():
        path.unlink()


def path_exists(path: Path) -> bool:
    return path.exists() or path.is_symlink()


def preflight_install_target(target: Path, *, force: bool) -> None:
    if target.is_symlink():
        raise ValueError(f"Install target must not be a symlink: {target}")
    if target.exists() and not force:
        raise FileExistsError(f"Install target already exists: {target}")


def preflight_install_parent(parent: Path) -> None:
    if parent.is_symlink():
        raise ValueError(f"Install parent must not be a symlink: {parent}")
    ancestor = parent
    while not path_exists(ancestor):
        ancestor = ancestor.parent
    if not ancestor.is_dir():
        raise NotADirectoryError(f"Install parent is not a directory: {ancestor}")


def make_executable(path: Path) -> None:
    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def write_install_state(
    state_path: Path,
    manifest: dict,
    *,
    app_dir: Path,
    code_mode_host_backup_path: Path | None,
    code_mode_host_path: Path | None,
    engine_backup_path: Path | None,
    shim_path: Path | None,
    supervisor_paths: SupervisorPaths,
    code_route: CodeRouteState | None,
) -> None:
    state_path = resolve_destination(state_path)
    code_mode_host_release = managed_engine_release_from_manifest(
        manifest
    ).code_mode_host
    state = {
        "appPath": str(app_dir),
        "artifacts": {
            role: {
                "fileName": entry["fileName"],
                "sha256": entry["sha256"],
                "sizeBytes": entry["sizeBytes"],
            }
            for role, entry in manifest["artifacts"].items()
        },
        "bundleVersion": manifest["bundleVersion"],
        "codeRoute": serialize_code_route_state(code_route),
        "codeModeHostBackupPath": str(code_mode_host_backup_path)
        if code_mode_host_backup_path is not None
        else None,
        "codeModeHostBackupSha256": sha256_file(code_mode_host_backup_path)
        if code_mode_host_backup_path is not None
        else None,
        "codeModeHostPath": str(code_mode_host_path)
        if code_mode_host_path is not None
        else None,
        "engineBackupPath": str(engine_backup_path)
        if engine_backup_path is not None
        else None,
        "engineBackupSha256": sha256_file(engine_backup_path)
        if engine_backup_path is not None
        else None,
        "enginePath": str(supervisor_paths.managed_cli),
        "managedEngine": {
            "sha256": manifest["managedEngine"]["sha256"],
            "signingIdentifier": manifest["managedEngine"]["signingIdentifier"],
            "teamIdentifier": manifest["managedEngine"]["teamIdentifier"],
        },
        "managedCodeModeHost": {
            "sha256": code_mode_host_release.sha256,
            "signingIdentifier": code_mode_host_release.signing_identifier,
            "teamIdentifier": code_mode_host_release.team_identifier,
        }
        if code_mode_host_release is not None
        else None,
        "labHome": str(supervisor_paths.lab_home),
        "launchAgentsDir": str(supervisor_paths.launch_agents_dir),
        "listenHost": supervisor_paths.listen_host,
        "listenPort": supervisor_paths.listen_port,
        "releaseTag": manifest["release"]["tag"],
        "releaseVersion": manifest_release_version(manifest),
        "shimPath": str(shim_path) if shim_path is not None else None,
        "source": manifest["source"],
        "supervisorLabel": supervisor_paths.label,
        "version": manifest["version"],
    }
    preflight_install_parent(state_path.parent)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    temp_path = state_path.with_suffix(f"{state_path.suffix}.tmp")
    with temp_path.open("w", encoding="utf-8") as handle:
        json.dump(state, handle, indent=2, sort_keys=True)
        handle.write("\n")
    temp_path.replace(state_path)


def manifest_release_version(manifest: dict) -> str:
    value = manifest.get("releaseVersion")
    if isinstance(value, str) and value:
        return value
    release = manifest.get("release")
    if not isinstance(release, dict):
        raise ValueError("Installer requires a published release manifest")
    tag = release.get("tag")
    if not isinstance(tag, str):
        raise ValueError("Installer release tag is missing")
    return release_identity_from_tag(tag)
