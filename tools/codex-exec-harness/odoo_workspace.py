#!/usr/bin/env python3
"""Launch Codex from a current generated Odoo workspace."""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
SUPPORTED_STATUS_SCHEMA_VERSION = 1
DEFAULT_STATUS_TIMEOUT_SECONDS = 120
PROVENANCE_TIMEOUT_SECONDS = 30
MAX_STATUS_DETAIL_ITEMS = 12
ACCESS_MODES = {"editable", "read-only"}
ANCESTOR_GUIDANCE_FILENAMES = ("AGENTS.override.md", "AGENTS.md")


class OdooWorkspaceError(Exception):
    """Raised when generated workspace evidence is unsafe or stale."""


@dataclass(frozen=True)
class WorkspaceSource:
    role: str
    workspace_relative_path: str
    workspace_entry_path: Path
    resolved_path: Path
    materialization: str
    editable: bool

    def evidence(self) -> dict[str, object]:
        return {
            "role": self.role,
            "workspace_relative_path": self.workspace_relative_path,
            "workspace_entry_path": str(self.workspace_entry_path),
            "resolved_path": str(self.resolved_path),
            "materialization": self.materialization,
            "editable": self.editable,
        }


@dataclass(frozen=True)
class OdooWorkspaceLaunch:
    workspace_path: Path
    manifest_path: Path
    agents_path: Path
    local_notes_path: Path
    git_root: Path | None
    status_command: tuple[str, ...]
    status_payload_sha256: str
    editable_sources: tuple[WorkspaceSource, ...]
    managed_sources: tuple[WorkspaceSource, ...]

    @property
    def writable_roots(self) -> tuple[Path, ...]:
        roots: list[Path] = []
        seen: set[Path] = set()
        for source in self.editable_sources:
            if source.resolved_path in seen:
                continue
            seen.add(source.resolved_path)
            roots.append(source.resolved_path)
        return tuple(roots)

    def evidence(self) -> dict[str, object]:
        return {
            "workspace_path": str(self.workspace_path),
            "manifest_path": str(self.manifest_path),
            "agents_path": str(self.agents_path),
            "local_notes_path": str(self.local_notes_path),
            "git_root": str(self.git_root) if self.git_root is not None else None,
            "non_git_workspace": self.git_root is None,
            "status_command": list(self.status_command),
            "status_payload_sha256": self.status_payload_sha256,
            "editable_sources": [source.evidence() for source in self.editable_sources],
            "managed_sources": [source.evidence() for source in self.managed_sources],
            "writable_roots": [str(path) for path in self.writable_roots],
        }


def _required_text(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise OdooWorkspaceError(f"{label} must be a non-empty string")
    return value.strip()


def _required_bool(payload: dict[str, Any], key: str) -> None:
    if payload.get(key) is not True:
        raise OdooWorkspaceError(f"workspace status requires {key}=true")


def _resolve_existing_directory(value: object, label: str) -> Path:
    path_text = _required_text(value, label)
    path = Path(path_text).expanduser()
    if not path.is_absolute():
        raise OdooWorkspaceError(f"{label} must be absolute: {path_text}")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise OdooWorkspaceError(f"{label} is unavailable: {path_text}: {error}") from error
    if not resolved.is_dir():
        raise OdooWorkspaceError(f"{label} is not a directory: {resolved}")
    if resolved == Path(resolved.anchor):
        raise OdooWorkspaceError(f"{label} cannot be the filesystem root")
    return resolved


def _absolute_existing_directory(value: object, label: str) -> Path:
    path_text = _required_text(value, label)
    raw_path = Path(os.path.abspath(os.path.expanduser(path_text)))
    if not raw_path.is_dir():
        raise OdooWorkspaceError(f"{label} is not a directory: {raw_path}")
    try:
        path = raw_path.parent.resolve(strict=True) / raw_path.name
    except OSError as error:
        raise OdooWorkspaceError(f"{label} is unavailable: {raw_path}: {error}") from error
    if path == Path(path.anchor):
        raise OdooWorkspaceError(f"{label} cannot be the filesystem root")
    return path


def _resolve_existing_file(value: object, label: str) -> Path:
    path_text = _required_text(value, label)
    path = Path(path_text).expanduser()
    if not path.is_absolute():
        raise OdooWorkspaceError(f"{label} must be absolute: {path_text}")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise OdooWorkspaceError(f"{label} is unavailable: {path_text}: {error}") from error
    if not resolved.is_file():
        raise OdooWorkspaceError(f"{label} is not a file: {resolved}")
    return resolved


def _workspace_relative_path(value: object, label: str) -> str:
    path_text = _required_text(value, label)
    path = Path(path_text)
    if path.is_absolute() or ".." in path.parts:
        raise OdooWorkspaceError(f"{label} must remain inside the generated workspace")
    return path.as_posix()


def _canonical_json_sha256(value: object) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def _paths_overlap(first: Path, second: Path) -> bool:
    return first == second or first.is_relative_to(second) or second.is_relative_to(first)


def _ancestor_guidance_paths(workspace_path: Path) -> tuple[Path, ...]:
    return tuple(
        candidate
        for ancestor in workspace_path.parents
        for filename in ANCESTOR_GUIDANCE_FILENAMES
        if (candidate := ancestor / filename).exists() or candidate.is_symlink()
    )


def _git_root(path: Path) -> Path | None:
    try:
        completed = subprocess.run(
            ["git", "-C", str(path), "rev-parse", "--show-toplevel"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        raise OdooWorkspaceError(f"could not verify non-Git workspace root: {error}") from error
    if completed.returncode == 0:
        root_text = completed.stdout.strip()
        if not root_text:
            raise OdooWorkspaceError("Git reported an empty workspace root")
        return Path(root_text).expanduser().resolve(strict=True)
    detail = (completed.stderr or completed.stdout).strip().lower()
    if "not a git repository" in detail:
        return None
    raise OdooWorkspaceError(
        "could not verify non-Git workspace root: "
        + ((completed.stderr or completed.stdout).strip() or f"git exited {completed.returncode}")
    )


def _parse_status_json(stdout: str) -> dict[str, Any]:
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise OdooWorkspaceError(f"workspace status emitted invalid JSON: {error}") from error
    if not isinstance(payload, dict):
        raise OdooWorkspaceError("workspace status must emit a JSON object")
    return payload


def _bounded_status_detail(payload: dict[str, Any], fallback: str) -> str:
    stale_reasons = payload.get("stale_reasons")
    if isinstance(stale_reasons, list):
        reasons = [str(item).strip() for item in stale_reasons if str(item).strip()]
        if reasons:
            bounded = reasons[:MAX_STATUS_DETAIL_ITEMS]
            suffix = "" if len(reasons) <= len(bounded) else ", ..."
            return f"stale reasons: {', '.join(bounded)}{suffix}"
    return fallback.strip() or "no diagnostic output"


def run_workspace_status(
    *,
    uv_bin: str,
    devkit_path: Path,
    manifest_path: Path,
    timeout_seconds: int = DEFAULT_STATUS_TIMEOUT_SECONDS,
) -> tuple[dict[str, Any], tuple[str, ...]]:
    if timeout_seconds <= 0:
        raise OdooWorkspaceError("workspace status timeout must be positive")
    resolved_uv = resolve_executable(uv_bin)
    resolved_devkit = devkit_path.expanduser().resolve(strict=True)
    resolved_manifest = manifest_path.expanduser().resolve(strict=True)
    if not resolved_devkit.is_dir():
        raise OdooWorkspaceError(f"devkit path is not a directory: {resolved_devkit}")
    if not resolved_manifest.is_file():
        raise OdooWorkspaceError(f"workspace manifest is not a file: {resolved_manifest}")
    command = (
        str(resolved_uv),
        "--directory",
        str(resolved_devkit),
        "run",
        "platform",
        "workspace",
        "status",
        "--manifest",
        str(resolved_manifest),
        "--check",
    )
    try:
        completed = subprocess.run(
            command,
            cwd=resolved_devkit,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as error:
        raise OdooWorkspaceError(
            f"workspace status --check timed out after {timeout_seconds}s"
        ) from error
    except OSError as error:
        raise OdooWorkspaceError(f"could not run workspace status --check: {error}") from error

    payload = _parse_status_json(completed.stdout)
    if completed.returncode != 0:
        detail = _bounded_status_detail(payload, completed.stderr)
        raise OdooWorkspaceError(
            f"workspace status --check failed with exit {completed.returncode}: {detail}"
        )
    return payload, command


def _source_records(payload: dict[str, Any], workspace_path: Path) -> tuple[WorkspaceSource, ...]:
    sources_value = payload.get("sources")
    if not isinstance(sources_value, list) or not sources_value:
        raise OdooWorkspaceError("workspace status requires a non-empty sources list")

    sources: list[WorkspaceSource] = []
    roles: set[str] = set()
    for index, raw_source in enumerate(sources_value):
        if not isinstance(raw_source, dict):
            raise OdooWorkspaceError(f"workspace source {index} must be an object")
        role = _required_text(raw_source.get("role"), f"workspace source {index} role")
        if role in roles:
            raise OdooWorkspaceError(f"workspace source role is duplicated: {role}")
        roles.add(role)
        relative_path = _workspace_relative_path(
            raw_source.get("workspace_relative_path"),
            f"workspace source {role} relative path",
        )
        resolved_path = _resolve_existing_directory(
            raw_source.get("resolved_path"), f"workspace source {role} resolved path"
        )
        workspace_entry = _absolute_existing_directory(
            raw_source.get("workspace_entry_path"),
            f"workspace source {role} entry path",
        )
        expected_entry = workspace_path / relative_path
        if workspace_entry != expected_entry:
            raise OdooWorkspaceError(
                f"workspace source {role} entry is redirected outside its declared workspace path"
            )
        try:
            expected_entry_resolved = expected_entry.resolve(strict=True)
        except OSError as error:
            raise OdooWorkspaceError(
                f"workspace source {role} entry is unavailable: {expected_entry}: {error}"
            ) from error
        if expected_entry_resolved != resolved_path:
            raise OdooWorkspaceError(
                f"workspace source {role} entry does not resolve to its declared source root"
            )
        materialization = _required_text(
            raw_source.get("materialization"), f"workspace source {role} materialization"
        )
        if materialization not in {"linked_path", "managed_checkout"}:
            raise OdooWorkspaceError(
                f"workspace source {role} has unsupported materialization: {materialization}"
            )
        editable = raw_source.get("editable")
        if type(editable) is not bool:
            raise OdooWorkspaceError(f"workspace source {role} editable must be boolean")
        if editable and materialization != "linked_path":
            raise OdooWorkspaceError(
                f"workspace source {role} cannot be editable when materialized as {materialization}"
            )
        if raw_source.get("materialization_current") is not True:
            raise OdooWorkspaceError(f"workspace source {role} materialization is not current")
        if raw_source.get("materialization_state") != "current":
            raise OdooWorkspaceError(f"workspace source {role} materialization state is not current")
        if materialization == "linked_path" and not workspace_entry.is_symlink():
            raise OdooWorkspaceError(f"workspace source {role} must be a linked path")
        if materialization == "managed_checkout" and workspace_entry.is_symlink():
            raise OdooWorkspaceError(f"workspace source {role} managed checkout cannot be a symlink")
        sources.append(
            WorkspaceSource(
                role=role,
                workspace_relative_path=relative_path,
                workspace_entry_path=workspace_entry,
                resolved_path=resolved_path,
                materialization=materialization,
                editable=editable,
            )
        )
    return tuple(sources)


def validate_workspace_status(
    payload: dict[str, Any],
    *,
    manifest_path: Path,
    status_command: tuple[str, ...],
) -> OdooWorkspaceLaunch:
    if type(payload.get("schema_version")) is not int or payload["schema_version"] != SUPPORTED_STATUS_SCHEMA_VERSION:
        raise OdooWorkspaceError("workspace status uses an unsupported schema version")
    for key in (
        "current",
        "workspace_exists",
        "lock_file_exists",
        "lock_file_current",
        "surface_current",
        "materialization_current",
        "managed_source_baseline_current",
    ):
        _required_bool(payload, key)

    workspace_path = _absolute_existing_directory(
        payload.get("workspace_path"), "workspace path"
    )
    if workspace_path.is_symlink():
        raise OdooWorkspaceError("workspace path must not be redirected through a symlink")
    git_root = _git_root(workspace_path)
    if git_root is not None:
        raise OdooWorkspaceError(
            f"generated workspace must be outside a Git work tree; found {git_root}"
        )
    ancestor_guidance = _ancestor_guidance_paths(workspace_path)
    if ancestor_guidance:
        raise OdooWorkspaceError(
            "generated workspace is shadowed by ancestor guidance: "
            + ", ".join(str(path) for path in ancestor_guidance)
        )
    agents_path = _resolve_existing_file(
        payload.get("workspace_agents_path"), "workspace AGENTS.md path"
    )
    expected_agents = workspace_path / "AGENTS.md"
    if agents_path != expected_agents.resolve(strict=True) or expected_agents.is_symlink():
        raise OdooWorkspaceError("canonical workspace AGENTS.md must be a regular root file")

    resolved_manifest = manifest_path.expanduser().resolve(strict=True)
    manifest = payload.get("manifest")
    if not isinstance(manifest, dict) or manifest.get("current") is not True:
        raise OdooWorkspaceError("workspace status requires a current manifest")
    reported_manifest = _resolve_existing_file(manifest.get("path"), "reported manifest path")
    if reported_manifest != resolved_manifest:
        raise OdooWorkspaceError("workspace status manifest does not match the requested manifest")
    manifest_sha256 = _required_text(manifest.get("sha256"), "reported manifest sha256")
    if _sha256_file(resolved_manifest) != manifest_sha256:
        raise OdooWorkspaceError("workspace manifest changed after status verification")

    reserved_override = payload.get("reserved_override")
    if not isinstance(reserved_override, dict):
        raise OdooWorkspaceError("workspace status requires reserved override evidence")
    override_path_text = _required_text(
        reserved_override.get("path"), "workspace reserved override path"
    )
    override_path = Path(override_path_text).expanduser()
    if not override_path.is_absolute():
        raise OdooWorkspaceError("workspace reserved override path must be absolute")
    override_path = override_path.parent.resolve(strict=True) / override_path.name
    if override_path != workspace_path / "AGENTS.override.md":
        raise OdooWorkspaceError("workspace reserved override path does not match the root contract")
    if type(reserved_override.get("exists")) is not bool:
        raise OdooWorkspaceError("workspace reserved override existence must be boolean")
    if (
        reserved_override["exists"] is not False
        or override_path.exists()
        or override_path.is_symlink()
    ):
        raise OdooWorkspaceError(
            "generated workspace is shadowed by reserved AGENTS.override.md; resync or remove it"
        )
    if reserved_override.get("semantics") != "full_replacement":
        raise OdooWorkspaceError("workspace status reported unknown override semantics")
    if reserved_override.get("allowed_in_normal_flow") is not False:
        raise OdooWorkspaceError("workspace status reported unsafe override semantics")

    local_notes = payload.get("local_notes")
    if not isinstance(local_notes, dict) or local_notes.get("valid") is not True:
        raise OdooWorkspaceError("workspace local notes are invalid")
    if type(local_notes.get("exists")) is not bool:
        raise OdooWorkspaceError("workspace local notes existence must be boolean")
    if local_notes.get("semantics") != "supplemental_non_secret_notes":
        raise OdooWorkspaceError("workspace status reported unknown local note semantics")
    local_notes_path_text = _required_text(
        local_notes.get("path"), "workspace local notes path"
    )
    local_notes_path = Path(local_notes_path_text).expanduser()
    if not local_notes_path.is_absolute():
        raise OdooWorkspaceError("workspace local notes path must be absolute")
    local_notes_path = local_notes_path.parent.resolve(strict=True) / local_notes_path.name
    expected_local_notes = workspace_path / "workspace.local.md"
    if local_notes_path != expected_local_notes:
        raise OdooWorkspaceError("workspace local notes path does not match the root contract")
    local_notes_exists = local_notes_path.exists() or local_notes_path.is_symlink()
    if local_notes_exists != local_notes["exists"]:
        raise OdooWorkspaceError("workspace local notes changed after status verification")
    if local_notes_exists and (local_notes_path.is_symlink() or not local_notes_path.is_file()):
        raise OdooWorkspaceError("workspace local notes must be a regular file when present")

    sources = _source_records(payload, workspace_path)
    source_by_role = {source.role: source for source in sources}
    edit_roots = payload.get("edit_roots")
    if not isinstance(edit_roots, list):
        raise OdooWorkspaceError("workspace status requires edit_roots")
    edit_roles: list[str] = []
    for index, raw_root in enumerate(edit_roots):
        if not isinstance(raw_root, dict):
            raise OdooWorkspaceError(f"workspace edit root {index} must be an object")
        role = _required_text(raw_root.get("role"), f"workspace edit root {index} role")
        source = source_by_role.get(role)
        if source is None or not source.editable:
            raise OdooWorkspaceError(f"workspace edit root {role} is not an editable source")
        resolved_path = _resolve_existing_directory(
            raw_root.get("resolved_path"), f"workspace edit root {role} resolved path"
        )
        relative_path = _workspace_relative_path(
            raw_root.get("workspace_relative_path"),
            f"workspace edit root {role} relative path",
        )
        if resolved_path != source.resolved_path or relative_path != source.workspace_relative_path:
            raise OdooWorkspaceError(f"workspace edit root {role} disagrees with source evidence")
        edit_roles.append(role)
    expected_edit_roles = [source.role for source in sources if source.editable]
    if edit_roles != expected_edit_roles:
        raise OdooWorkspaceError(
            "workspace edit roots do not exactly match status-declared editable sources"
        )

    editable_sources = tuple(source for source in sources if source.editable)
    read_only_sources = tuple(source for source in sources if not source.editable)
    for editable_source in editable_sources:
        if _paths_overlap(editable_source.resolved_path, workspace_path):
            raise OdooWorkspaceError(
                f"workspace editable source {editable_source.role} overlaps the generated workspace"
            )
        for read_only_source in read_only_sources:
            if _paths_overlap(editable_source.resolved_path, read_only_source.resolved_path):
                raise OdooWorkspaceError(
                    "workspace editable source "
                    f"{editable_source.role} overlaps read-only source {read_only_source.role}"
                )

    return OdooWorkspaceLaunch(
        workspace_path=workspace_path,
        manifest_path=resolved_manifest,
        agents_path=agents_path,
        local_notes_path=local_notes_path,
        git_root=git_root,
        status_command=status_command,
        status_payload_sha256=_canonical_json_sha256(payload),
        editable_sources=editable_sources,
        managed_sources=read_only_sources,
    )


def inspect_odoo_workspace(
    *,
    uv_bin: str,
    devkit_path: Path,
    manifest_path: Path,
    timeout_seconds: int = DEFAULT_STATUS_TIMEOUT_SECONDS,
) -> tuple[OdooWorkspaceLaunch, dict[str, Any]]:
    payload, command = run_workspace_status(
        uv_bin=uv_bin,
        devkit_path=devkit_path,
        manifest_path=manifest_path,
        timeout_seconds=timeout_seconds,
    )
    return (
        validate_workspace_status(
            payload,
            manifest_path=manifest_path,
            status_command=command,
        ),
        payload,
    )


def resolve_executable(value: str) -> Path:
    requested = Path(value).expanduser()
    if requested.parent != Path(".") or requested.is_absolute():
        path = requested.resolve(strict=True)
    else:
        discovered = shutil.which(value)
        if discovered is None:
            raise OdooWorkspaceError(f"could not find executable: {value}")
        path = Path(discovered).resolve(strict=True)
    if not path.is_file():
        raise OdooWorkspaceError(f"executable path is not a file: {path}")
    if not os.access(path, os.X_OK):
        raise OdooWorkspaceError(f"executable path is not executable: {path}")
    return path


def build_codex_command(
    *,
    launch: OdooWorkspaceLaunch,
    codex_bin: Path,
    mode: str,
    access: str,
    prompt: str | None,
    model: str | None = None,
    config_profile: str | None = None,
    auth_profile: str | None = None,
) -> tuple[str, ...]:
    if mode not in {"exec", "interactive"}:
        raise OdooWorkspaceError(f"unsupported launch mode: {mode}")
    if access not in ACCESS_MODES:
        raise OdooWorkspaceError(f"unsupported access mode: {access}")
    if mode == "exec" and (prompt is None or not prompt.strip()):
        raise OdooWorkspaceError("exec mode requires a prompt")

    command = [str(codex_bin)]
    if mode == "exec":
        command.extend(("exec", "--json", "--skip-git-repo-check"))
    command.extend(("-C", str(launch.workspace_path)))
    if model:
        command.extend(("--model", model))
    if config_profile:
        command.extend(("--profile", config_profile))
    if auth_profile:
        command.extend(("--auth-profile", auth_profile))
    if access == "read-only":
        command.extend(("--sandbox", "read-only"))
    else:
        if not launch.writable_roots:
            raise OdooWorkspaceError("editable mode requires at least one declared edit root")
        command.extend(("--sandbox", "workspace-write"))
        for writable_root in launch.writable_roots:
            command.extend(("--workspace-root", str(writable_root)))
    if prompt is not None and prompt.strip():
        command.extend(("--", prompt))
    return tuple(command)


def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_codex_provenance(*, source_repo: Path, codex_bin: Path) -> dict[str, Any]:
    resolved_source = source_repo.expanduser().resolve(strict=True)
    script = resolved_source / "scripts" / "local" / "codex_lab_provenance.py"
    if not script.is_file() or script.is_symlink():
        raise OdooWorkspaceError(f"Codex provenance verifier is unavailable: {script}")
    command = [
        sys.executable,
        str(script),
        "--repo-root",
        str(resolved_source),
        "--binary",
        str(codex_bin),
        "--verify-only",
        "--json",
    ]
    try:
        completed = subprocess.run(
            command,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=PROVENANCE_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        raise OdooWorkspaceError("Codex provenance verification timed out") from error
    except OSError as error:
        raise OdooWorkspaceError(f"could not run Codex provenance verification: {error}") from error
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout).strip() or "no diagnostic output"
        raise OdooWorkspaceError(f"Codex provenance verification failed: {detail}")
    try:
        report = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise OdooWorkspaceError(f"Codex provenance verifier emitted invalid JSON: {error}") from error
    if not isinstance(report, dict) or report.get("status") != "current":
        raise OdooWorkspaceError(
            "Codex binary provenance is not current: "
            + json.dumps(report, sort_keys=True, separators=(",", ":"))
        )
    return report


def _redacted_command(command: tuple[str, ...], prompt: str | None) -> list[str]:
    redacted = list(command)
    if prompt is not None and prompt.strip() and redacted and redacted[-1] == prompt:
        redacted[-1] = "<prompt>"
    return redacted


def build_evidence(
    *,
    launch: OdooWorkspaceLaunch,
    codex_bin: Path,
    provenance: dict[str, Any],
    command: tuple[str, ...],
    mode: str,
    access: str,
    prompt: str | None,
    returncode: int | None,
) -> dict[str, object]:
    return {
        "schema_version": SCHEMA_VERSION,
        "generated_at": datetime.now(UTC).isoformat(),
        "status": "planned" if returncode is None else ("pass" if returncode == 0 else "fail"),
        "mode": mode,
        "access": access,
        "command": _redacted_command(command, prompt),
        "prompt_sha256": (
            hashlib.sha256(prompt.encode("utf-8")).hexdigest()
            if prompt is not None and prompt.strip()
            else None
        ),
        "returncode": returncode,
        "codex_binary": {
            "path": str(codex_bin),
            "sha256": _sha256_file(codex_bin),
            "provenance": provenance,
        },
        "workspace": launch.evidence(),
        "permissions": {
            "profile": ":workspace" if access == "editable" else ":read-only",
            "workspace_root_writable": False,
            "writable_roots": (
                [str(path) for path in launch.writable_roots]
                if access == "editable"
                else []
            ),
        },
        "guidance": {
            "path": str(launch.agents_path),
            "sha256": _sha256_file(launch.agents_path),
            "local_notes_exists": launch.local_notes_path.is_file(),
            "reserved_override_exists": (launch.workspace_path / "AGENTS.override.md").exists(),
        },
    }


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--devkit", required=True, help="Path to the odoo-devkit checkout")
    parser.add_argument("--manifest", required=True, help="Path to the tenant workspace.toml")
    parser.add_argument("--uv-bin", default="uv", help="uv executable used for workspace status")
    parser.add_argument("--codex-bin", required=True, help="Exact Codex Lab candidate binary")
    parser.add_argument("--source-repo", required=True, help="Codex Lab source repo for provenance")
    parser.add_argument("--mode", choices=("exec", "interactive"), default="exec")
    parser.add_argument("--access", choices=tuple(sorted(ACCESS_MODES)), default="editable")
    parser.add_argument("--model", default=None)
    parser.add_argument("--config-profile", default=None)
    parser.add_argument("--auth-profile", default=None)
    parser.add_argument("--prompt", default=None)
    parser.add_argument("--status-timeout-seconds", type=int, default=DEFAULT_STATUS_TIMEOUT_SECONDS)
    parser.add_argument("--evidence-file", default=None)
    parser.add_argument("--dry-run", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        launch, _payload = inspect_odoo_workspace(
            uv_bin=args.uv_bin,
            devkit_path=Path(args.devkit),
            manifest_path=Path(args.manifest),
            timeout_seconds=args.status_timeout_seconds,
        )
        codex_bin = resolve_executable(args.codex_bin)
        provenance = verify_codex_provenance(
            source_repo=Path(args.source_repo), codex_bin=codex_bin
        )
        command = build_codex_command(
            launch=launch,
            codex_bin=codex_bin,
            mode=args.mode,
            access=args.access,
            prompt=args.prompt,
            model=args.model,
            config_profile=args.config_profile,
            auth_profile=args.auth_profile,
        )
        planned_evidence = build_evidence(
            launch=launch,
            codex_bin=codex_bin,
            provenance=provenance,
            command=command,
            mode=args.mode,
            access=args.access,
            prompt=args.prompt,
            returncode=None,
        )
        if args.dry_run:
            if args.evidence_file:
                write_json(Path(args.evidence_file), planned_evidence)
            print(json.dumps(planned_evidence, indent=2, sort_keys=True))
            return 0

        completed = subprocess.run(command, cwd=launch.workspace_path)
        final_evidence = build_evidence(
            launch=launch,
            codex_bin=codex_bin,
            provenance=provenance,
            command=command,
            mode=args.mode,
            access=args.access,
            prompt=args.prompt,
            returncode=completed.returncode,
        )
        if args.evidence_file:
            write_json(Path(args.evidence_file), final_evidence)
        return completed.returncode
    except (OdooWorkspaceError, OSError, ValueError) as error:
        print(f"odoo-workspace: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
