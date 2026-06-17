"""Install Codex Lab release artifacts from a distribution manifest."""

from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
import json
import re
import shutil
import stat
import tempfile
import urllib.parse
import urllib.request
import uuid
import zipfile

from .distribution_manifest import APP_ZIP
from .distribution_manifest import ARTIFACTS
from .distribution_manifest import MANIFEST_NAME
from .distribution_manifest import SHIM_ZIP
from .distribution_manifest import is_https_url
from .distribution_manifest import read_sha256sums
from .distribution_manifest import validate_manifest
from .layout import build_shim_script
from .smoke import smoke_check


DEFAULT_REPOSITORY = "cbusillo/codex-lab"
DEFAULT_APP_DIR = Path.home() / "Applications" / "Codex Lab.app"
DEFAULT_SHIM_DIR = Path.home() / ".local" / "bin"
DEFAULT_STATE_PATH = (
    Path.home() / "Library" / "Application Support" / "Codex Lab" / "install-state.json"
)
SHA256SUMS_NAME = "SHA256SUMS"
USER_AGENT = "codex-lab-installer/0"
DOWNLOAD_TIMEOUT_SECONDS = 60
LAB_RELEASE_TAG_PREFIX = "codex-lab-v"
LAB_RELEASE_ORDER_PATTERN = re.compile(
    r"^codex-lab-v(?P<major>[0-9]+)\.(?P<minor>[0-9]+)\.(?P<patch>[0-9]+)"
    r"(?:-lab\.(?P<lab>[0-9]+))?$"
)
MAX_LATEST_RELEASE_PAGES = 10

DownloadFunc = Callable[[str, Path], None]


class CodexLabInstallStateError(Exception):
    pass


class CodexLabUpdateError(Exception):
    pass


@dataclass(frozen=True)
class CodexLabReleaseSummary:
    published_at: str
    tag_name: str


@dataclass(frozen=True)
class CodexLabInstallResult:
    app_dir: Path
    release_tag: str
    shim_path: Path | None
    state_path: Path
    version: str


@dataclass(frozen=True)
class CodexLabInstallStatus:
    app_path: Path
    bundle_version: str
    release_tag: str
    shim_path: Path | None
    source_commit: str | None
    state_path: Path
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
class Replacement:
    target: Path
    backup_path: Path


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
    force: bool = False,
    download: DownloadFunc | None = None,
) -> CodexLabInstallResult:
    if not is_https_url(manifest_url):
        raise ValueError(f"manifest URL must be an HTTPS URL: {manifest_url}")
    download = download or download_url

    app_dir = resolve_destination(app_dir)
    shim_path = resolve_destination(shim_dir / "codex-lab") if shim_dir else None
    state_path = resolve_destination(state_path)

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
        smoke_check(app_source, shim_source)

        preflight_install_parent(app_dir.parent)
        preflight_install_target(app_dir, force=force)
        if shim_path is not None:
            preflight_install_parent(shim_path.parent)
            preflight_install_target(shim_path, force=force)
        preflight_install_parent(state_path.parent)

        replacements = []
        installed_shim = None
        try:
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
            write_install_state(
                state_path,
                manifest,
                app_dir=installed_app,
                shim_path=installed_shim,
            )
        except Exception:
            rollback_replacements(replacements)
            raise
        cleanup_replacements(replacements)
        return CodexLabInstallResult(
            app_dir=installed_app,
            release_tag=manifest["release"]["tag"],
            shim_path=installed_shim,
            state_path=state_path,
            version=manifest["version"],
        )


def read_install_state(state_path: Path = DEFAULT_STATE_PATH) -> CodexLabInstallStatus:
    state_path = resolve_destination(state_path)
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
    if isinstance(source, dict):
        commit = source.get("commit")
        if isinstance(commit, str):
            source_commit = commit

    shim_path = state.get("shimPath")
    if shim_path is not None and not isinstance(shim_path, str):
        raise CodexLabInstallStateError(
            f"Install state field shimPath must be a string or null: {state_path}"
        )
    return CodexLabInstallStatus(
        app_path=Path(required_state_string(state, "appPath", state_path)),
        bundle_version=required_state_string(state, "bundleVersion", state_path),
        release_tag=required_state_string(state, "releaseTag", state_path),
        shim_path=Path(shim_path) if isinstance(shim_path, str) else None,
        source_commit=source_commit,
        state_path=state_path,
        version=required_state_string(state, "version", state_path),
    )


def check_for_update(
    *,
    repository: str = DEFAULT_REPOSITORY,
    state_path: Path = DEFAULT_STATE_PATH,
) -> CodexLabUpdateCheck:
    installed = read_install_state(state_path)
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
) -> CodexLabUpdateResult:
    check = check_for_update(repository=repository, state_path=state_path)
    if not check.update_available:
        return CodexLabUpdateResult(check=check, install=None)

    manifest_url = manifest_url_for_release_tag(
        check.latest_release_tag,
        repository=repository,
    )
    installed = check.installed
    return CodexLabUpdateResult(
        check=check,
        install=install_from_manifest_url(
            manifest_url,
            app_dir=installed.app_path,
            shim_dir=installed.shim_path.parent
            if installed.shim_path is not None
            else None,
            state_path=installed.state_path,
            force=True,
            download=download,
        ),
    )


def required_state_string(state: dict, field: str, state_path: Path) -> str:
    value = state.get(field)
    if not isinstance(value, str) or not value:
        raise CodexLabInstallStateError(
            f"Install state field {field} must be a non-empty string: {state_path}"
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


def codex_lab_release_order(tag_name: str) -> tuple[int, int, int, int, int] | None:
    match = LAB_RELEASE_ORDER_PATTERN.fullmatch(tag_name)
    if match is None:
        return None
    major = int(match.group("major"))
    minor = int(match.group("minor"))
    patch = int(match.group("patch"))
    lab = match.group("lab")
    if lab is None:
        return (major, minor, patch, 1, 0)
    return (major, minor, patch, 0, int(lab))


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
) -> Replacement:
    target.parent.mkdir(parents=True, exist_ok=True)
    backup_path = backup_path_for(target)
    if target.exists() or target.is_symlink():
        preflight_install_target(target, force=force)
        target.rename(backup_path)

    try:
        shutil.move(str(source), str(target))
    except Exception:
        remove_path(target)
        if backup_path.exists() or backup_path.is_symlink():
            shutil.move(str(backup_path), str(target))
        raise
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
        if replacement.backup_path.exists() or replacement.backup_path.is_symlink():
            shutil.move(str(replacement.backup_path), str(replacement.target))


def cleanup_replacements(replacements: list[Replacement]) -> None:
    for replacement in replacements:
        remove_path(replacement.backup_path)


def remove_path(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    elif path.exists() or path.is_symlink():
        path.unlink()


def preflight_install_target(target: Path, *, force: bool) -> None:
    if target.is_symlink():
        raise ValueError(f"Install target must not be a symlink: {target}")
    if target.exists() and not force:
        raise FileExistsError(f"Install target already exists: {target}")


def preflight_install_parent(parent: Path) -> None:
    if parent.is_symlink():
        raise ValueError(f"Install parent must not be a symlink: {parent}")


def make_executable(path: Path) -> None:
    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def write_install_state(
    state_path: Path,
    manifest: dict,
    *,
    app_dir: Path,
    shim_path: Path | None,
) -> None:
    state_path = resolve_destination(state_path)
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
        "releaseTag": manifest["release"]["tag"],
        "shimPath": str(shim_path) if shim_path is not None else None,
        "source": manifest["source"],
        "version": manifest["version"],
    }
    preflight_install_parent(state_path.parent)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    temp_path = state_path.with_suffix(f"{state_path.suffix}.tmp")
    with temp_path.open("w", encoding="utf-8") as handle:
        json.dump(state, handle, indent=2, sort_keys=True)
        handle.write("\n")
    temp_path.replace(state_path)
