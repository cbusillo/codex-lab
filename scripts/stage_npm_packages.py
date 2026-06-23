#!/usr/bin/env python3
"""Stage one or more Codex npm packages for release."""

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import contextmanager
from dataclasses import dataclass
import importlib.util
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path
from typing import Sequence


REPO_ROOT = Path(__file__).resolve().parent.parent
BUILD_SCRIPT = REPO_ROOT / "codex-cli" / "scripts" / "build_npm_package.py"
GITHUB_REPO = "openai/codex"
BINARY_TARGETS = (
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
)
ARTIFACT_CACHE_MARKER = ".stage-npm-artifact.json"

_SPEC = importlib.util.spec_from_file_location("codex_build_npm_package", BUILD_SCRIPT)
if _SPEC is None or _SPEC.loader is None:
    raise RuntimeError(f"Unable to load module from {BUILD_SCRIPT}")
_BUILD_MODULE = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_BUILD_MODULE)
PACKAGE_NATIVE_COMPONENTS = getattr(_BUILD_MODULE, "PACKAGE_NATIVE_COMPONENTS", {})
PACKAGE_EXPANSIONS = getattr(_BUILD_MODULE, "PACKAGE_EXPANSIONS", {})
CODEX_PLATFORM_PACKAGES = getattr(_BUILD_MODULE, "CODEX_PLATFORM_PACKAGES", {})
CODEX_PACKAGE_COMPONENT = getattr(
    _BUILD_MODULE, "CODEX_PACKAGE_COMPONENT", "codex-package"
)


@dataclass(frozen=True)
class BinaryComponent:
    artifact_prefix: str
    dest_dir: str
    binary_basename: str


@dataclass(frozen=True)
class WorkflowArtifact:
    name: str
    size_in_bytes: int


@dataclass(frozen=True)
class ReleaseAsset:
    name: str
    size_in_bytes: int
    asset_id: str = ""
    digest: str = ""
    updated_at: str = ""


@dataclass(frozen=True)
class ReleaseArtifact:
    name: str
    size_in_bytes: int
    assets: tuple[ReleaseAsset, ...]

    @property
    def asset_names(self) -> tuple[str, ...]:
        return tuple(asset.name for asset in self.assets)


@dataclass(frozen=True)
class StagedPackage:
    package: str
    pack_output: Path
    output: str


class PackageStageError(RuntimeError):
    def __init__(self, package: str, output: str):
        super().__init__(f"Failed to stage npm package {package}")
        self.package = package
        self.output = output


BINARY_COMPONENTS = {
    "codex-responses-api-proxy": BinaryComponent(
        artifact_prefix="codex-responses-api-proxy",
        dest_dir="codex-responses-api-proxy",
        binary_basename="codex-responses-api-proxy",
    ),
}


def _gha_enabled() -> bool:
    return os.environ.get("GITHUB_ACTIONS") == "true"


def _gha_escape(value: str) -> str:
    return value.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


@contextmanager
def _gha_group(title: str):
    if _gha_enabled():
        print(f"::group::{_gha_escape(title)}", flush=True)
    try:
        yield
    finally:
        if _gha_enabled():
            print("::endgroup::", flush=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--release-version",
        required=True,
        help="Version to stage (e.g. 0.1.0 or 0.1.0-alpha.1).",
    )
    parser.add_argument(
        "--package",
        dest="packages",
        action="append",
        required=True,
        help="Package name to stage. May be provided multiple times.",
    )
    parser.add_argument(
        "--workflow-url",
        help=(
            "Optional workflow URL to reuse for native artifacts. When omitted, "
            "native artifacts are downloaded from GitHub release assets."
        ),
    )
    parser.add_argument(
        "--release-tag",
        help="GitHub release tag to use for native artifacts (default: rust-v<release-version>).",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=None,
        help="Directory where npm tarballs should be written (default: dist/npm).",
    )
    parser.add_argument(
        "--artifacts-cache-dir",
        type=Path,
        default=None,
        help="Directory used to cache downloaded native artifacts.",
    )
    parser.add_argument(
        "--keep-staging-dirs",
        action="store_true",
        help="Retain temporary staging directories instead of deleting them.",
    )
    return parser.parse_args()


def native_components_for_package(package: str) -> tuple[str, ...]:
    return tuple(sorted(PACKAGE_NATIVE_COMPONENTS.get(package, [])))


def collect_native_component_sets(packages: list[str]) -> list[tuple[str, ...]]:
    component_sets: list[tuple[str, ...]] = []
    seen: set[tuple[str, ...]] = set()
    for package in packages:
        components = native_components_for_package(package)
        if not components or components in seen:
            continue
        seen.add(components)
        component_sets.append(components)
    return component_sets


def expand_packages(packages: list[str]) -> list[str]:
    expanded: list[str] = []
    for package in packages:
        for expanded_package in PACKAGE_EXPANSIONS.get(package, [package]):
            if expanded_package in expanded:
                continue
            expanded.append(expanded_package)
    return expanded


def resolve_release_tag(version: str, override: str | None) -> str:
    if override:
        return override
    return f"rust-v{version}"


def install_native_components(
    release_tag: str,
    workflow_url: str | None,
    components: set[str],
    vendor_root: Path,
    artifacts_dir: Path,
) -> None:
    if not components:
        return

    vendor_dir = vendor_root / "vendor"
    vendor_dir.mkdir(parents=True, exist_ok=True)

    artifacts_dir.mkdir(parents=True, exist_ok=True)
    if workflow_url:
        workflow_id = workflow_url.rstrip("/").split("/")[-1]
        print(
            f"Downloading native artifacts from workflow {workflow_id}...", flush=True
        )
        with _gha_group(f"Download native artifacts from workflow {workflow_id}"):
            install_from_workflow_artifacts(
                workflow_id,
                artifacts_dir,
                sorted(components),
                vendor_dir,
            )
    else:
        print(f"Downloading native artifacts from release {release_tag}...", flush=True)
        with _gha_group(f"Download native artifacts from release {release_tag}"):
            install_from_release_assets(
                release_tag,
                artifacts_dir,
                sorted(components),
                vendor_dir,
            )
    print(f"Installed native dependencies into {vendor_dir}", flush=True)


def install_from_release_assets(
    release_tag: str,
    artifacts_dir: Path,
    components: Sequence[str],
    vendor_dir: Path,
) -> None:
    artifacts = select_release_artifacts(release_tag, components)
    download_release_artifacts(release_tag, artifacts_dir, artifacts)
    if CODEX_PACKAGE_COMPONENT in components:
        install_codex_package_archives(artifacts_dir, vendor_dir, BINARY_TARGETS)
    install_binary_components(
        artifacts_dir,
        vendor_dir,
        [BINARY_COMPONENTS[name] for name in components if name in BINARY_COMPONENTS],
    )


def select_release_artifacts(
    release_tag: str,
    components: Sequence[str],
) -> list[ReleaseArtifact]:
    needs_target_artifacts = CODEX_PACKAGE_COMPONENT in components or any(
        component in BINARY_COMPONENTS for component in components
    )
    if not needs_target_artifacts:
        return []

    assets_by_name = {asset.name: asset for asset in list_release_assets(release_tag)}
    selected_artifacts: list[ReleaseArtifact] = []
    for target in BINARY_TARGETS:
        asset_names: list[str] = []
        if CODEX_PACKAGE_COMPONENT in components:
            asset_names.append(f"codex-package-{target}.tar.gz")
        for component_name in components:
            component = BINARY_COMPONENTS.get(component_name)
            if component is not None:
                asset_names.append(
                    archive_name_for_target(component.artifact_prefix, target)
                )

        selected_assets: list[ReleaseAsset] = []
        for asset_name in asset_names:
            asset = assets_by_name.get(asset_name)
            if asset is None:
                raise FileNotFoundError(
                    f"Expected release asset not found for {release_tag}: {asset_name}"
                )
            selected_assets.append(asset)

        selected_artifacts.append(
            ReleaseArtifact(
                name=target,
                size_in_bytes=sum(asset.size_in_bytes for asset in selected_assets),
                assets=tuple(selected_assets),
            )
        )

    return selected_artifacts


def list_release_assets(release_tag: str) -> list[ReleaseAsset]:
    stdout = subprocess.check_output(
        [
            "gh",
            "release",
            "view",
            release_tag,
            "--repo",
            GITHUB_REPO,
            "--json",
            "assets",
        ],
        text=True,
    )
    payload = json.loads(stdout)
    assets: list[ReleaseAsset] = []
    for asset in payload.get("assets", []):
        assets.append(
            ReleaseAsset(
                name=asset["name"],
                size_in_bytes=int(asset["size"]),
                asset_id=str(asset.get("id", "")),
                digest=str(asset.get("digest", "")),
                updated_at=str(asset.get("updatedAt", "")),
            )
        )
    return assets


def download_release_artifacts(
    release_tag: str,
    dest_dir: Path,
    artifacts: Sequence[ReleaseArtifact],
) -> None:
    total_bytes = sum(artifact.size_in_bytes for artifact in artifacts)
    print(
        f"Downloading {len(artifacts)} release artifact sets ({format_bytes(total_bytes)})",
        flush=True,
    )
    source_id = f"github-release:{GITHUB_REPO}:{release_tag}"
    for artifact in artifacts:
        artifact_dir = dest_dir / artifact.name
        if release_artifact_cache_is_complete(artifact_dir, source_id, artifact):
            print(
                f"  using cached {artifact.name} ({format_bytes(artifact.size_in_bytes)})",
                flush=True,
            )
            continue

        if artifact_dir.exists():
            shutil.rmtree(artifact_dir)
        artifact_dir.mkdir(parents=True, exist_ok=True)
        print(
            f"  downloading {artifact.name} ({format_bytes(artifact.size_in_bytes)})",
            flush=True,
        )
        for asset_name in artifact.asset_names:
            subprocess.check_call(
                [
                    "gh",
                    "release",
                    "download",
                    release_tag,
                    "--repo",
                    GITHUB_REPO,
                    "--pattern",
                    asset_name,
                    "--dir",
                    str(artifact_dir),
                    "--clobber",
                ]
            )
        write_release_artifact_cache_marker(artifact_dir, source_id, artifact)


def release_artifact_cache_is_complete(
    artifact_dir: Path,
    source_id: str,
    artifact: ReleaseArtifact,
) -> bool:
    marker_path = artifact_dir / ARTIFACT_CACHE_MARKER
    if not artifact_dir.is_dir() or not marker_path.is_file():
        return False

    try:
        marker = json.loads(marker_path.read_text())
    except (OSError, json.JSONDecodeError):
        return False

    if marker != release_artifact_cache_marker(source_id, artifact):
        return False

    return all(
        (artifact_dir / asset_name).is_file() for asset_name in artifact.asset_names
    )


def write_release_artifact_cache_marker(
    artifact_dir: Path,
    source_id: str,
    artifact: ReleaseArtifact,
) -> None:
    marker_path = artifact_dir / ARTIFACT_CACHE_MARKER
    marker_path.write_text(
        json.dumps(release_artifact_cache_marker(source_id, artifact), sort_keys=True)
        + "\n"
    )


def release_artifact_cache_marker(
    source_id: str,
    artifact: ReleaseArtifact,
) -> dict[str, int | list[dict[str, int | str]] | str]:
    return {
        "assets": [release_asset_cache_marker(asset) for asset in artifact.assets],
        "name": artifact.name,
        "size_in_bytes": artifact.size_in_bytes,
        "source_id": source_id,
    }


def release_asset_cache_marker(asset: ReleaseAsset) -> dict[str, int | str]:
    return {
        "digest": asset.digest,
        "id": asset.asset_id,
        "name": asset.name,
        "size_in_bytes": asset.size_in_bytes,
        "updated_at": asset.updated_at,
    }


def install_from_workflow_artifacts(
    workflow_id: str,
    artifacts_dir: Path,
    components: Sequence[str],
    vendor_dir: Path,
) -> None:
    artifacts = select_target_artifacts(workflow_id, components)
    download_artifacts(workflow_id, artifacts_dir, artifacts)
    if CODEX_PACKAGE_COMPONENT in components:
        install_codex_package_archives(artifacts_dir, vendor_dir, BINARY_TARGETS)
    install_binary_components(
        artifacts_dir,
        vendor_dir,
        [BINARY_COMPONENTS[name] for name in components if name in BINARY_COMPONENTS],
    )


def select_target_artifacts(
    workflow_id: str,
    components: Sequence[str],
) -> list[WorkflowArtifact]:
    needs_target_artifacts = CODEX_PACKAGE_COMPONENT in components or any(
        component in BINARY_COMPONENTS for component in components
    )
    if not needs_target_artifacts:
        return []

    artifacts_by_name = {
        artifact.name: artifact for artifact in list_workflow_artifacts(workflow_id)
    }
    selected_artifacts: list[WorkflowArtifact] = []
    for target in BINARY_TARGETS:
        for artifact_name in [target, f"{target}-unsigned"]:
            artifact = artifacts_by_name.get(artifact_name)
            if artifact is not None:
                selected_artifacts.append(artifact)
                break
        else:
            raise FileNotFoundError(
                f"Expected workflow artifact not found for target {target}"
            )

    return selected_artifacts


def list_workflow_artifacts(workflow_id: str) -> list[WorkflowArtifact]:
    stdout = subprocess.check_output(
        [
            "gh",
            "api",
            f"repos/{GITHUB_REPO}/actions/runs/{workflow_id}/artifacts",
            "--paginate",
            "--jq",
            ".artifacts[] | [.name, .size_in_bytes] | @tsv",
        ],
        text=True,
    )
    artifacts: list[WorkflowArtifact] = []
    for line in stdout.splitlines():
        name, size_in_bytes = line.split("\t", 1)
        artifacts.append(WorkflowArtifact(name=name, size_in_bytes=int(size_in_bytes)))
    return artifacts


def download_artifacts(
    workflow_id: str,
    dest_dir: Path,
    artifacts: Sequence[WorkflowArtifact],
) -> None:
    total_bytes = sum(artifact.size_in_bytes for artifact in artifacts)
    print(
        f"Downloading {len(artifacts)} artifacts ({format_bytes(total_bytes)})",
        flush=True,
    )
    for artifact in artifacts:
        artifact_dir = dest_dir / artifact.name
        if artifact_cache_is_complete(artifact_dir, workflow_id, artifact):
            print(
                f"  using cached {artifact.name} ({format_bytes(artifact.size_in_bytes)})",
                flush=True,
            )
            continue

        if artifact_dir.exists():
            shutil.rmtree(artifact_dir)
        artifact_dir.mkdir(parents=True, exist_ok=True)
        print(
            f"  downloading {artifact.name} ({format_bytes(artifact.size_in_bytes)})",
            flush=True,
        )
        subprocess.check_call(
            [
                "gh",
                "run",
                "download",
                "--name",
                artifact.name,
                "--dir",
                str(artifact_dir),
                "--repo",
                GITHUB_REPO,
                workflow_id,
            ]
        )
        write_artifact_cache_marker(artifact_dir, workflow_id, artifact)


def artifact_cache_is_complete(
    artifact_dir: Path,
    workflow_id: str,
    artifact: WorkflowArtifact,
) -> bool:
    marker_path = artifact_dir / ARTIFACT_CACHE_MARKER
    if not artifact_dir.is_dir() or not marker_path.is_file():
        return False

    try:
        marker = json.loads(marker_path.read_text())
    except (OSError, json.JSONDecodeError):
        return False

    if marker != artifact_cache_marker(workflow_id, artifact):
        return False

    return any(path.name != ARTIFACT_CACHE_MARKER for path in artifact_dir.iterdir())


def write_artifact_cache_marker(
    artifact_dir: Path,
    workflow_id: str,
    artifact: WorkflowArtifact,
) -> None:
    marker_path = artifact_dir / ARTIFACT_CACHE_MARKER
    marker_path.write_text(
        json.dumps(artifact_cache_marker(workflow_id, artifact), sort_keys=True) + "\n"
    )


def artifact_cache_marker(
    workflow_id: str,
    artifact: WorkflowArtifact,
) -> dict[str, int | str]:
    return {
        "name": artifact.name,
        "size_in_bytes": artifact.size_in_bytes,
        "workflow_id": workflow_id,
    }


def install_codex_package_archives(
    artifacts_dir: Path,
    vendor_dir: Path,
    targets: Sequence[str],
) -> None:
    if not targets:
        return

    print(
        "Installing Codex package archives for targets: " + ", ".join(targets),
        flush=True,
    )
    max_workers = min(len(targets), max(1, (os.cpu_count() or 1)))
    with ThreadPoolExecutor(max_workers=max_workers) as executor:
        futures = {
            executor.submit(
                install_single_codex_package_archive,
                artifacts_dir,
                vendor_dir,
                target,
            ): target
            for target in targets
        }
        for future in as_completed(futures):
            installed_path = future.result()
            print(f"  installed {installed_path}", flush=True)


def install_single_codex_package_archive(
    artifacts_dir: Path,
    vendor_dir: Path,
    target: str,
) -> Path:
    artifact_subdir = artifact_dir_for_target(artifacts_dir, target)
    archive_path = artifact_subdir / f"codex-package-{target}.tar.gz"
    if not archive_path.exists():
        raise FileNotFoundError(f"Expected package archive not found: {archive_path}")

    dest_dir = vendor_dir / target
    if dest_dir.exists():
        shutil.rmtree(dest_dir)
    dest_dir.mkdir(parents=True, exist_ok=True)

    with tarfile.open(archive_path, "r:gz") as archive:
        archive.extractall(dest_dir, filter="data")

    return dest_dir


def install_binary_components(
    artifacts_dir: Path,
    vendor_dir: Path,
    selected_components: Sequence[BinaryComponent],
) -> None:
    for component in selected_components:
        component_targets = list(BINARY_TARGETS)

        print(
            f"Installing {component.binary_basename} binaries for targets: "
            + ", ".join(component_targets),
            flush=True,
        )
        max_workers = min(len(component_targets), max(1, (os.cpu_count() or 1)))
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            futures = {
                executor.submit(
                    install_single_binary,
                    artifacts_dir,
                    vendor_dir,
                    target,
                    component,
                ): target
                for target in component_targets
            }
            for future in as_completed(futures):
                installed_path = future.result()
                print(f"  installed {installed_path}", flush=True)


def install_single_binary(
    artifacts_dir: Path,
    vendor_dir: Path,
    target: str,
    component: BinaryComponent,
) -> Path:
    artifact_subdir = artifact_dir_for_target(artifacts_dir, target)
    archive_path = binary_archive_path(
        artifact_subdir, component.artifact_prefix, target
    )

    dest_dir = vendor_dir / target / component.dest_dir
    dest_dir.mkdir(parents=True, exist_ok=True)

    binary_name = (
        f"{component.binary_basename}.exe"
        if "windows" in target
        else component.binary_basename
    )
    dest = dest_dir / binary_name
    dest.unlink(missing_ok=True)
    extract_zstd_archive(archive_path, dest)
    if "windows" not in target:
        dest.chmod(0o755)
    return dest


def binary_archive_path(artifact_dir: Path, artifact_prefix: str, target: str) -> Path:
    archive_names = [archive_name_for_target(artifact_prefix, target)]
    if artifact_dir.name == f"{target}-unsigned":
        archive_names.append(
            archive_name_for_target(artifact_prefix, f"{target}-unsigned")
        )

    for archive_name in archive_names:
        archive_path = artifact_dir / archive_name
        if archive_path.exists():
            return archive_path

    raise FileNotFoundError(
        f"Expected artifact not found: {artifact_dir / archive_names[0]}"
    )


def archive_name_for_target(artifact_prefix: str, target: str) -> str:
    if "windows" in target:
        return f"{artifact_prefix}-{target}.exe.zst"
    return f"{artifact_prefix}-{target}.zst"


def artifact_dir_for_target(artifacts_dir: Path, target: str) -> Path:
    for artifact_name in [target, f"{target}-unsigned"]:
        artifact_dir = artifacts_dir / artifact_name
        if artifact_dir.is_dir():
            return artifact_dir

    return artifacts_dir / target


def extract_zstd_archive(archive_path: Path, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)

    output_path = archive_path.parent / dest.name
    subprocess.check_call(
        ["zstd", "-f", "-d", str(archive_path), "-o", str(output_path)]
    )
    shutil.move(str(output_path), dest)


def format_bytes(size_in_bytes: int) -> str:
    value = float(size_in_bytes)
    for unit in ["B", "KiB", "MiB"]:
        if value < 1024:
            return f"{value:.1f} {unit}"
        value /= 1024
    return f"{value:.1f} GiB"


def run_command(cmd: list[str]) -> str:
    output = "+ " + " ".join(cmd) + "\n"
    result = subprocess.run(
        cmd,
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )
    output += result.stdout
    if result.returncode != 0:
        raise subprocess.CalledProcessError(
            result.returncode,
            cmd,
            output=output,
        )
    return output


def tarball_name_for_package(package: str, version: str) -> str:
    if package in CODEX_PLATFORM_PACKAGES:
        platform = package.removeprefix("codex-")
        return f"codex-npm-{platform}-{version}.tgz"
    return f"{package}-npm-{version}.tgz"


def stage_single_package(
    package: str,
    release_version: str,
    output_dir: Path,
    runner_temp: Path,
    vendor_src: Path | None,
    keep_staging_dirs: bool,
) -> StagedPackage:
    staging_dir = Path(
        tempfile.mkdtemp(prefix=f"npm-stage-{package}-", dir=runner_temp)
    )
    pack_output = output_dir / tarball_name_for_package(package, release_version)
    lines = [f"Staging {package} in {staging_dir}"]

    cmd = [
        str(BUILD_SCRIPT),
        "--package",
        package,
        "--release-version",
        release_version,
        "--staging-dir",
        str(staging_dir),
        "--pack-output",
        str(pack_output),
    ]

    if vendor_src is not None:
        cmd.extend(["--vendor-src", str(vendor_src)])

    try:
        lines.append(run_command(cmd).rstrip())
    except subprocess.CalledProcessError as error:
        output = error.output if isinstance(error.output, str) else str(error)
        lines.append(output.rstrip())
        raise PackageStageError(
            package, "\n".join(line for line in lines if line)
        ) from error
    finally:
        if not keep_staging_dirs:
            shutil.rmtree(staging_dir, ignore_errors=True)

    return StagedPackage(
        package=package,
        pack_output=pack_output,
        output="\n".join(line for line in lines if line),
    )


def main() -> int:
    args = parse_args()

    output_dir = args.output_dir or (REPO_ROOT / "dist" / "npm")
    output_dir.mkdir(parents=True, exist_ok=True)

    runner_temp = Path(os.environ.get("RUNNER_TEMP", tempfile.gettempdir()))

    packages = expand_packages(list(args.packages))
    native_component_sets = collect_native_component_sets(packages)
    print("Expanded packages: " + ", ".join(packages), flush=True)
    if native_component_sets:
        component_sets = [
            "(" + ", ".join(components) + ")" for components in native_component_sets
        ]
        print(
            "Native component sets: " + ", ".join(component_sets),
            flush=True,
        )

    vendor_temp_roots: list[Path] = []
    vendor_src_by_components: dict[tuple[str, ...], Path] = {}
    artifacts_root: Path | None = None
    cleanup_artifacts_root = False

    final_messages = []

    try:
        if native_component_sets:
            workflow_url: str | None = None
            release_tag = resolve_release_tag(args.release_version, args.release_tag)
            if args.workflow_url:
                workflow_url = args.workflow_url
                print(f"Using native artifacts from {workflow_url}", flush=True)
            else:
                print(f"Using native artifacts from {release_tag}", flush=True)
            if args.artifacts_cache_dir is not None:
                artifacts_root = args.artifacts_cache_dir
                artifacts_root.mkdir(parents=True, exist_ok=True)
            else:
                artifacts_root = Path(
                    tempfile.mkdtemp(prefix="npm-native-artifacts-", dir=runner_temp)
                )
                cleanup_artifacts_root = True
            print(f"Caching downloaded artifacts in {artifacts_root}", flush=True)
            for components in native_component_sets:
                vendor_temp_root = Path(
                    tempfile.mkdtemp(prefix="npm-native-", dir=runner_temp)
                )
                vendor_temp_roots.append(vendor_temp_root)
                print(
                    "Installing native components "
                    + ", ".join(components)
                    + f" into {vendor_temp_root}",
                    flush=True,
                )
                install_native_components(
                    release_tag,
                    workflow_url,
                    set(components),
                    vendor_temp_root,
                    artifacts_root,
                )
                vendor_src_by_components[components] = vendor_temp_root / "vendor"

        max_workers = min(len(packages), max(1, os.cpu_count() or 1))
        staged_by_package: dict[str, StagedPackage] = {}
        errors_by_package: dict[str, PackageStageError] = {}
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            futures = {
                executor.submit(
                    stage_single_package,
                    package,
                    args.release_version,
                    output_dir,
                    runner_temp,
                    vendor_src_by_components.get(
                        native_components_for_package(package)
                    ),
                    args.keep_staging_dirs,
                ): package
                for package in packages
            }
            for future in as_completed(futures):
                package = futures[future]
                try:
                    staged_by_package[package] = future.result()
                except PackageStageError as error:
                    errors_by_package[package] = error

        for package in packages:
            staged = staged_by_package.get(package)
            output = (
                staged.output
                if staged is not None
                else errors_by_package[package].output
            )
            with _gha_group(f"Stage {package}"):
                print(output, flush=True)

        if errors_by_package:
            failed_packages = ", ".join(
                package for package in packages if package in errors_by_package
            )
            raise RuntimeError(f"Failed to stage npm package(s): {failed_packages}")

        for package in packages:
            staged = staged_by_package[package]
            final_messages.append(f"Staged {package} at {staged.pack_output}")
    finally:
        if not args.keep_staging_dirs:
            for vendor_temp_root in vendor_temp_roots:
                shutil.rmtree(vendor_temp_root, ignore_errors=True)
        if cleanup_artifacts_root and artifacts_root is not None:
            shutil.rmtree(artifacts_root, ignore_errors=True)

    for msg in final_messages:
        print(msg, flush=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
