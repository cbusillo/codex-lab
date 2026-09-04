"""Manage the explicit, provenance-pinned ``code`` command route."""

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile

from .layout import MAX_PROVENANCE_BYTES


DEFAULT_CODE_ROUTE_PATH = Path.home() / ".local" / "bin" / "code"
CODE_ROUTE_SCHEMA_VERSION = 1
PRIOR_BACKUP_PREFIX = ".code.codex-lab-prior-"
ACTIVE_BACKUP_PREFIX = ".code.codex-lab-active-"


class CodeRouteRecoveryError(ValueError):
    pass


@dataclass(frozen=True)
class CodeRouteEngine:
    path: Path
    sha256: str
    signing_identifier: str
    source_commit: str
    team_identifier: str
    release_tag: str
    release_version: str
    version: str
    build_channel: str
    lab_home: Path


@dataclass(frozen=True)
class PriorCodeRoute:
    kind: str
    backup_path: Path | None = None
    mode: int | None = None
    sha256: str | None = None
    symlink_target: str | None = None


@dataclass(frozen=True)
class CodeRouteState:
    active_path: Path
    engine: CodeRouteEngine
    launcher_sha256: str
    prior: PriorCodeRoute


@dataclass(frozen=True)
class CodeRouteResult:
    active_path: Path
    changed: bool
    restored_prior: bool
    state_path: Path


@dataclass(frozen=True)
class LauncherTools:
    awk: Path = Path("/usr/bin/awk")
    codesign: Path = Path("/usr/bin/codesign")
    env: Path = Path("/usr/bin/env")
    mktemp: Path = Path("/usr/bin/mktemp")
    plutil: Path = Path("/usr/bin/plutil")
    rm: Path = Path("/bin/rm")
    shasum: Path = Path("/usr/bin/shasum")
    tr: Path = Path("/usr/bin/tr")
    wc: Path = Path("/usr/bin/wc")


def build_code_route_launcher_script(
    engine: CodeRouteEngine,
    *,
    tools: LauncherTools = LauncherTools(),
) -> str:
    values = {
        "ENGINE": str(engine.path),
        "EXPECTED_ENGINE_SHA256": engine.sha256,
        "EXPECTED_SIGNING_IDENTIFIER": engine.signing_identifier,
        "EXPECTED_SOURCE_COMMIT": engine.source_commit,
        "EXPECTED_TEAM_IDENTIFIER": engine.team_identifier,
        "EXPECTED_RELEASE_VERSION": engine.release_version,
        "EXPECTED_VERSION": engine.version,
        "EXPECTED_BUILD_CHANNEL": engine.build_channel,
        "LAB_HOME": str(engine.lab_home),
        "AWK": str(tools.awk),
        "CODESIGN": str(tools.codesign),
        "ENV": str(tools.env),
        "MKTEMP": str(tools.mktemp),
        "PLUTIL": str(tools.plutil),
        "RM": str(tools.rm),
        "SHASUM": str(tools.shasum),
        "TR": str(tools.tr),
        "WC": str(tools.wc),
    }
    assignments = "\n".join(
        f"{name}={shell_quote(value)}" for name, value in values.items()
    )
    return f"""#!/bin/sh
set -eu

{assignments}

fail() {{
  echo "Codex Lab code route verification failed: $1" >&2
  exit 1
}}

[ -f "$ENGINE" ] || fail "managed engine is missing: $ENGINE"
[ -x "$ENGINE" ] || fail "managed engine is not executable: $ENGINE"
[ ! -L "$ENGINE" ] || fail "managed engine must not be a symlink: $ENGINE"
engine_dir=${{ENGINE%/*}}
engine_name=${{ENGINE##*/}}
physical_engine_dir=$(CDPATH= cd -P "$engine_dir" && pwd) \
  || fail "managed engine parent could not be resolved"
[ "$physical_engine_dir/$engine_name" = "$ENGINE" ] \
  || fail "managed engine path does not match the pinned executable path"

"$CODESIGN" --verify --strict "$ENGINE" >/dev/null 2>&1 \
  || fail "managed engine signature is invalid"
signature=$("$CODESIGN" -dvvv "$ENGINE" 2>&1) \
  || fail "managed engine signature identity is unavailable"
signing_identifier=$(printf '%s\n' "$signature" | "$AWK" -F= \
  '$1 == "Identifier" {{ print substr($0, index($0, "=") + 1); exit }}')
team_identifier=$(printf '%s\n' "$signature" | "$AWK" -F= \
  '$1 == "TeamIdentifier" {{ print substr($0, index($0, "=") + 1); exit }}')
[ "$signing_identifier" = "$EXPECTED_SIGNING_IDENTIFIER" ] \
  || fail "managed engine signing identifier does not match the activated route"
[ "$team_identifier" = "$EXPECTED_TEAM_IDENTIFIER" ] \
  || fail "managed engine signing team does not match the activated route"

actual_engine_sha256=$("$SHASUM" -a 256 "$ENGINE" | "$AWK" '{{ print $1 }}') \
  || fail "managed engine SHA-256 could not be computed"
[ "$actual_engine_sha256" = "$EXPECTED_ENGINE_SHA256" ] \
  || fail "managed engine SHA-256 does not match the activated route"

PROVENANCE_FILE=$("$MKTEMP" "${{TMPDIR:-/tmp}}/codex-lab-code-provenance.XXXXXX") \
  || fail "could not allocate a provenance file"
trap '"$RM" -f "$PROVENANCE_FILE"' EXIT HUP INT TERM
"$ENGINE" debug provenance --json >"$PROVENANCE_FILE" \
  || fail "managed engine did not report provenance"
provenance_bytes=$("$WC" -c <"$PROVENANCE_FILE" | "$TR" -d '[:space:]')
case "$provenance_bytes" in
  ''|*[!0-9]*) fail "managed engine provenance size could not be read" ;;
esac
[ "$provenance_bytes" -gt 0 ] && [ "$provenance_bytes" -le {MAX_PROVENANCE_BYTES} ] \
  || fail "managed engine provenance has an unsafe size"

provenance_field() {{
  "$PLUTIL" -extract "$1" raw -o - "$PROVENANCE_FILE" 2>/dev/null || true
}}

[ "$(provenance_field schema_version)" = "2" ] \
  || fail "managed engine provenance schema is unsupported"
[ "$(provenance_field executable_path)" = "$ENGINE" ] \
  || fail "managed engine provenance path does not match the pinned executable path"
[ "$(provenance_field source_commit)" = "$EXPECTED_SOURCE_COMMIT" ] \
  || fail "managed engine source commit does not match the activated route"
[ "$(provenance_field release_version)" = "$EXPECTED_RELEASE_VERSION" ] \
  || fail "managed engine release identity does not match the activated route"
[ "$(provenance_field version)" = "$EXPECTED_VERSION" ] \
  || fail "managed engine version does not match the activated route"
[ "$(provenance_field compatibility_version)" = "$EXPECTED_VERSION" ] \
  || fail "managed engine compatibility version does not match the activated route"
[ "$(provenance_field dirty_state)" = "clean" ] \
  || fail "managed engine provenance is not clean"
[ "$(provenance_field build_profile)" = "release" ] \
  || fail "managed engine build profile is not release"
[ "$(provenance_field build_channel)" = "$EXPECTED_BUILD_CHANNEL" ] \
  || fail "managed engine build channel does not match the activated route"

exec "$ENV" CODEX_HOME="$LAB_HOME" CODEX_LAB_HOME="$LAB_HOME" "$ENGINE" "$@"
"""


def activate_code_route(
    state_path: Path,
    engine: CodeRouteEngine,
    *,
    active_path: Path = DEFAULT_CODE_ROUTE_PATH,
    tools: LauncherTools = LauncherTools(),
    lock_held: bool = False,
) -> CodeRouteResult:
    from .code_route_transaction import activate_code_route as activate_transaction

    return activate_transaction(
        state_path,
        engine,
        active_path=active_path,
        tools=tools,
        lock_held=lock_held,
    )


def deactivate_code_route(
    state_path: Path,
    *,
    active_path: Path = DEFAULT_CODE_ROUTE_PATH,
    lock_held: bool = False,
) -> CodeRouteResult:
    from .code_route_transaction import deactivate_code_route as deactivate_transaction

    return deactivate_transaction(
        state_path,
        active_path=active_path,
        lock_held=lock_held,
    )


def recover_code_route_transaction(
    state_path: Path,
    *,
    active_path: Path = DEFAULT_CODE_ROUTE_PATH,
    lock_held: bool = False,
) -> None:
    from .code_route_transaction import (
        recover_code_route_transaction as recover_transaction,
    )

    recover_transaction(
        state_path,
        active_path=active_path,
        lock_held=lock_held,
    )


def require_active_code_route(
    route: CodeRouteState,
    *,
    expected_path: Path,
) -> None:
    expected_path = absolute_path(expected_path)
    if route.active_path != expected_path:
        raise ValueError(
            "Recorded active code route path does not match the requested route: "
            f"{route.active_path} != {expected_path}"
        )
    require_safe_parent(route.active_path.parent)
    mode = lstat_mode(route.active_path, "active code route")
    if not stat.S_ISREG(mode) or route.active_path.is_symlink():
        raise ValueError(
            f"Recorded active code route is not a regular file: {route.active_path}"
        )
    if sha256_file(route.active_path) != route.launcher_sha256:
        raise ValueError("Recorded active code route launcher has changed")
    require_prior_route(route.prior, route.active_path)


def capture_prior_metadata(
    active_path: Path,
    *,
    backup_path: Path | None = None,
) -> PriorCodeRoute:
    if not path_exists(active_path):
        return PriorCodeRoute(kind="absent")
    mode = lstat_mode(active_path, "existing code route")
    backup_path = backup_path or allocate_backup_path(
        active_path.parent, PRIOR_BACKUP_PREFIX
    )
    if stat.S_ISLNK(mode):
        return PriorCodeRoute(
            kind="symlink",
            backup_path=backup_path,
            symlink_target=os.readlink(active_path),
        )
    if stat.S_ISREG(mode):
        return PriorCodeRoute(
            kind="regular",
            backup_path=backup_path,
            mode=stat.S_IMODE(mode),
            sha256=sha256_file(active_path),
        )
    raise ValueError(
        f"Existing code route must be absent, a symlink, or a regular file: {active_path}"
    )


def require_prior_route(prior: PriorCodeRoute, active_path: Path) -> None:
    if prior.kind == "absent":
        if any(
            value is not None
            for value in (
                prior.backup_path,
                prior.mode,
                prior.sha256,
                prior.symlink_target,
            )
        ):
            raise ValueError("Recorded absent prior code route has unexpected metadata")
        return
    backup_path = prior.backup_path
    if backup_path is None:
        raise ValueError("Recorded prior code route backup path is missing")
    if backup_path.parent != active_path.parent or not backup_path.name.startswith(
        PRIOR_BACKUP_PREFIX
    ):
        raise ValueError("Recorded prior code route backup path is unsafe")
    mode = lstat_mode(backup_path, "prior code route backup")
    if prior.kind == "symlink":
        if not stat.S_ISLNK(mode) or os.readlink(backup_path) != prior.symlink_target:
            raise ValueError("Recorded prior code route symlink has changed")
        return
    if prior.kind == "regular":
        if stat.S_ISLNK(mode) or not stat.S_ISREG(mode):
            raise ValueError("Recorded prior code route backup is not a regular file")
        if stat.S_IMODE(mode) != prior.mode or sha256_file(backup_path) != prior.sha256:
            raise ValueError("Recorded prior code route file has changed")
        return
    raise ValueError(f"Recorded prior code route kind is unsupported: {prior.kind}")


def serialize_code_route_state(route: CodeRouteState | None) -> dict | None:
    if route is None:
        return None
    prior: dict[str, object] = {"kind": route.prior.kind}
    if route.prior.backup_path is not None:
        prior["backupPath"] = str(route.prior.backup_path)
    if route.prior.mode is not None:
        prior["mode"] = route.prior.mode
    if route.prior.sha256 is not None:
        prior["sha256"] = route.prior.sha256
    if route.prior.symlink_target is not None:
        prior["target"] = route.prior.symlink_target
    engine = route.engine
    return {
        "activePath": str(route.active_path),
        "engine": {
            "buildChannel": engine.build_channel,
            "labHome": str(engine.lab_home),
            "path": str(engine.path),
            "releaseTag": engine.release_tag,
            "releaseVersion": engine.release_version,
            "sha256": engine.sha256,
            "signingIdentifier": engine.signing_identifier,
            "sourceCommit": engine.source_commit,
            "teamIdentifier": engine.team_identifier,
            "version": engine.version,
        },
        "launcherSha256": route.launcher_sha256,
        "priorRoute": prior,
        "schemaVersion": CODE_ROUTE_SCHEMA_VERSION,
    }


def read_code_route_state(state: dict, state_path: Path) -> CodeRouteState | None:
    value = state.get("codeRoute")
    if value is None:
        return None
    if not isinstance(value, dict) or value.get("schemaVersion") != 1:
        raise ValueError(f"Install state codeRoute is unsupported: {state_path}")
    engine_value = required_dict(value, "engine", state_path)
    prior_value = required_dict(value, "priorRoute", state_path)
    prior_kind = required_string(prior_value, "kind", state_path)
    prior = PriorCodeRoute(
        kind=prior_kind,
        backup_path=optional_path(prior_value, "backupPath", state_path),
        mode=optional_int(prior_value, "mode", state_path),
        sha256=optional_sha256(prior_value, "sha256", state_path),
        symlink_target=optional_string(prior_value, "target", state_path),
    )
    return CodeRouteState(
        active_path=Path(required_string(value, "activePath", state_path)),
        engine=CodeRouteEngine(
            path=Path(required_string(engine_value, "path", state_path)),
            sha256=required_sha256(engine_value, "sha256", state_path),
            signing_identifier=required_string(
                engine_value, "signingIdentifier", state_path
            ),
            source_commit=required_string(engine_value, "sourceCommit", state_path),
            team_identifier=required_string(engine_value, "teamIdentifier", state_path),
            release_tag=required_string(engine_value, "releaseTag", state_path),
            release_version=required_string(engine_value, "releaseVersion", state_path),
            version=required_string(engine_value, "version", state_path),
            build_channel=required_string(engine_value, "buildChannel", state_path),
            lab_home=Path(required_string(engine_value, "labHome", state_path)),
        ),
        launcher_sha256=required_sha256(value, "launcherSha256", state_path),
        prior=prior,
    )


def write_state_code_route(
    state_path: Path,
    state: dict,
    route: CodeRouteState | None,
    *,
    expected_sha256: str | None = None,
) -> None:
    updated = dict(state)
    updated["codeRoute"] = serialize_code_route_state(route)
    require_safe_parent(state_path.parent)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    require_safe_parent(state_path.parent)
    with tempfile.NamedTemporaryFile(
        "w",
        dir=state_path.parent,
        encoding="utf-8",
        prefix=f".{state_path.name}.",
        suffix=".tmp",
        delete=False,
    ) as handle:
        temp_path = Path(handle.name)
        json.dump(updated, handle, indent=2, sort_keys=True)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    try:
        require_safe_parent(state_path.parent)
        if expected_sha256 is not None and sha256_file(state_path) != expected_sha256:
            raise CodeRouteRecoveryError(
                "Install state changed while the code route transaction was in progress"
            )
        temp_path.replace(state_path)
        from .code_route_transaction import fsync_directory

        fsync_directory(state_path.parent)
    except Exception:
        remove_path(temp_path)
        raise


def read_state_document(state_path: Path) -> dict:
    value, _sha256 = read_state_document_with_sha256(state_path)
    return value


def read_state_document_with_sha256(state_path: Path) -> tuple[dict, str]:
    content = state_path.read_bytes()
    value = json.loads(content)
    if not isinstance(value, dict):
        raise ValueError(f"Install state must be a JSON object: {state_path}")
    return value, hashlib.sha256(content).hexdigest()


def require_exact_engine_path(path: Path) -> None:
    if not path.is_absolute() or path.resolve(strict=True) != path:
        raise ValueError(f"Managed engine path is not exact and canonical: {path}")
    mode = lstat_mode(path, "managed engine")
    if stat.S_ISLNK(mode) or not stat.S_ISREG(mode) or not os.access(path, os.X_OK):
        raise ValueError(f"Managed engine is not a regular executable: {path}")


def require_safe_parent(parent: Path) -> None:
    parent = absolute_path(parent)
    current = parent
    missing: list[Path] = []
    while not path_exists(current):
        missing.append(current)
        if current == current.parent:
            break
        current = current.parent
    while True:
        mode = lstat_mode(current, "code route parent")
        if stat.S_ISLNK(mode) or not stat.S_ISDIR(mode):
            raise ValueError(f"Code route parent path is unsafe: {current}")
        if current == current.parent:
            break
        current = current.parent
    for path in missing:
        if path.is_symlink():
            raise ValueError(f"Code route parent path is unsafe: {path}")


def allocate_backup_path(parent: Path, prefix: str) -> Path:
    import uuid

    for _ in range(100):
        candidate = parent / f"{prefix}{uuid.uuid4().hex}"
        if not path_exists(candidate):
            return candidate
    raise FileExistsError(f"Could not allocate code route backup under {parent}")


def lstat_mode(path: Path, description: str) -> int:
    try:
        return path.lstat().st_mode
    except FileNotFoundError as exc:
        raise ValueError(f"Recorded {description} is missing: {path}") from exc


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def path_exists(path: Path) -> bool:
    return path.exists() or path.is_symlink()


def remove_path(path: Path) -> None:
    if path_exists(path):
        path.unlink()


def absolute_path(path: Path) -> Path:
    return Path(os.path.abspath(path.expanduser()))


def shell_quote(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


def required_dict(value: dict, field: str, state_path: Path) -> dict:
    result = value.get(field)
    if not isinstance(result, dict):
        raise ValueError(
            f"Install state codeRoute field {field} must be an object: {state_path}"
        )
    return result


def required_string(value: dict, field: str, state_path: Path) -> str:
    result = value.get(field)
    if not isinstance(result, str) or not result:
        raise ValueError(
            f"Install state codeRoute field {field} must be a non-empty string: {state_path}"
        )
    return result


def optional_string(value: dict, field: str, state_path: Path) -> str | None:
    result = value.get(field)
    if result is not None and not isinstance(result, str):
        raise ValueError(
            f"Install state codeRoute field {field} must be a string or null: {state_path}"
        )
    return result


def optional_path(value: dict, field: str, state_path: Path) -> Path | None:
    result = optional_string(value, field, state_path)
    return Path(result) if result is not None else None


def optional_int(value: dict, field: str, state_path: Path) -> int | None:
    result = value.get(field)
    if result is not None and (not isinstance(result, int) or result < 0):
        raise ValueError(
            f"Install state codeRoute field {field} must be a non-negative integer or null: {state_path}"
        )
    return result


def required_sha256(value: dict, field: str, state_path: Path) -> str:
    result = required_string(value, field, state_path)
    if len(result) != 64 or any(
        character not in "0123456789abcdef" for character in result
    ):
        raise ValueError(
            f"Install state codeRoute field {field} must be a lowercase SHA-256: {state_path}"
        )
    return result


def optional_sha256(value: dict, field: str, state_path: Path) -> str | None:
    result = optional_string(value, field, state_path)
    if result is not None and (
        len(result) != 64
        or any(character not in "0123456789abcdef" for character in result)
    ):
        raise ValueError(
            f"Install state codeRoute field {field} must be a lowercase SHA-256 or null: {state_path}"
        )
    return result
