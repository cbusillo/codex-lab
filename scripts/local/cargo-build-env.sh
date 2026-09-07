#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
repo_name="$(git -C "$repo_root" config --get remote.origin.url 2>/dev/null | sed -E 's#/*$##; s#\.git$##; s#^.*/##; s#^.*:##' || true)"
if [[ -z "$repo_name" ]]; then
	repo_name="codex-lab"
fi
artifact_root="${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}"
expected_volume_uuid="${CODEX_LAB_DEVELOPER_ARTIFACTS_VOLUME_UUID:-}"

report_storage_route() {
	printf 'cargo build storage: %s\n' "$1" >&2
}

validate_artifact_root() {
	case "$artifact_root" in
	/*) ;;
	*)
		printf 'configured artifact root must be an absolute path: %q\n' "$artifact_root" >&2
		exit 1
		;;
	esac

	if [[ ! -d "$artifact_root" ]]; then
		printf 'configured artifact root is missing: %q\n' "$artifact_root" >&2
		exit 1
	fi
	if [[ ! -w "$artifact_root" ]]; then
		printf 'configured artifact root is not writable: %q\n' "$artifact_root" >&2
		exit 1
	fi

	if [[ "$(uname -s)" != "Darwin" ]]; then
		if [[ -n "$expected_volume_uuid" ]]; then
			printf 'cannot verify configured artifact volume UUID %q outside macOS\n' "$expected_volume_uuid" >&2
			exit 1
		fi
		artifact_root_kind="path"
		return
	fi
	if ! command -v diskutil >/dev/null 2>&1 || ! command -v plutil >/dev/null 2>&1; then
		printf 'cannot verify configured artifact volume mount: diskutil and plutil are required\n' >&2
		exit 1
	fi

	local physical_artifact_root=""
	physical_artifact_root="$(cd "$artifact_root" && pwd -P)"
	local volume_plist=""
	volume_plist="$(diskutil info -plist "$physical_artifact_root" 2>/dev/null || true)"
	local mount_point=""
	mount_point="$(printf '%s' "$volume_plist" | plutil -extract MountPoint raw -o - - 2>/dev/null || true)"
	if [[ -z "$mount_point" ]]; then
		printf 'configured artifact root is not a mounted volume root: %q\n' "$artifact_root" >&2
		exit 1
	fi
	local physical_mount_point=""
	physical_mount_point="$(cd "$mount_point" 2>/dev/null && pwd -P || true)"
	if [[ "$physical_artifact_root" != "$physical_mount_point" ]]; then
		printf 'configured artifact root resolves to %q, but its mounted volume root is %q\n' \
			"$physical_artifact_root" "$mount_point" >&2
		exit 1
	fi
	artifact_root="$physical_artifact_root"
	artifact_root_kind="volume"

	if [[ -z "$expected_volume_uuid" ]]; then
		return
	fi
	local actual_volume_uuid=""
	actual_volume_uuid="$(printf '%s' "$volume_plist" | plutil -extract VolumeUUID raw -o - - 2>/dev/null || true)"
	if [[ -z "$actual_volume_uuid" ]]; then
		printf 'cannot determine volume UUID for configured artifact root: %q\n' "$artifact_root" >&2
		exit 1
	fi
	local normalized_actual=""
	local normalized_expected=""
	normalized_actual="$(printf '%s' "$actual_volume_uuid" | tr '[:lower:]' '[:upper:]')"
	normalized_expected="$(printf '%s' "$expected_volume_uuid" | tr '[:lower:]' '[:upper:]')"
	if [[ "$normalized_actual" != "$normalized_expected" ]]; then
		printf 'configured artifact root is on volume UUID %q; expected %q: %q\n' \
			"$actual_volume_uuid" "$expected_volume_uuid" "$artifact_root" >&2
		exit 1
	fi
}

host_triple() {
	local host=""
	if command -v rustc >/dev/null 2>&1; then
		local rustc_version=""
		rustc_version="$(rustc -vV)"
		host="$(awk '/^host:/ { print $2; exit }' <<<"$rustc_version")"
	else
		host="$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"
	fi
	printf '%s' "${host:-unknown-host}"
}

hash_text() {
	local value="$1"
	if command -v shasum >/dev/null 2>&1; then
		printf '%s' "$value" | shasum -a 256 | awk '{ print substr($1, 1, 12) }'
	elif command -v sha256sum >/dev/null 2>&1; then
		printf '%s' "$value" | sha256sum | awk '{ print substr($1, 1, 12) }'
	else
		printf '%s' "$value" | cksum | awk '{ print substr($1, 1, 12) }'
	fi
}

safe_name() {
	local value="$1"
	value="$(printf '%s' "$value" | tr -c '[:alnum:]._+-' '-' | sed -E 's/^-+//; s/-+$//; s/-+/-/g')"
	if [[ "$value" == "." || "$value" == ".." ]]; then
		value="workspace"
	fi
	printf '%s' "${value:-workspace}"
}

cargo_target_key() {
	if [[ -n "${CODEX_LAB_CARGO_TARGET_KEY:-}" ]]; then
		safe_name "$CODEX_LAB_CARGO_TARGET_KEY"
		return
	fi

	local branch=""
	branch="$(git -C "$repo_root" branch --show-current 2>/dev/null || true)"
	local slug=""
	slug="$(safe_name "${branch:-$(basename "$repo_root")}")"
	local repo_hash=""
	repo_hash="$(hash_text "$repo_root")"
	printf '%s-%s' "$slug" "$repo_hash"
}

if [[ -n "${CODEX_LAB_CARGO_TARGET_DIR:-}" ]]; then
	target_dir="$CODEX_LAB_CARGO_TARGET_DIR"
	report_storage_route "using explicit CODEX_LAB_CARGO_TARGET_DIR (artifact root unmanaged)"
elif [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
	if [[ -n "${CODEX_LAB_CARGO_TARGET_SCOPE:-}" && "${CODEX_LAB_CARGO_TARGET_SCOPE:-}" != "shared" ]]; then
		printf 'warning: CARGO_TARGET_DIR is already set; ignoring CODEX_LAB_CARGO_TARGET_SCOPE=%q\n' "$CODEX_LAB_CARGO_TARGET_SCOPE" >&2
	fi
	target_dir="$CARGO_TARGET_DIR"
	report_storage_route "using explicit CARGO_TARGET_DIR (artifact root unmanaged)"
elif [[ -n "$artifact_root" ]]; then
	artifact_root_kind=""
	validate_artifact_root
	host="$(host_triple)"
	target_scope="${CODEX_LAB_CARGO_TARGET_SCOPE:-worktree}"
	case "$target_scope" in
	shared)
		target_dir="${artifact_root%/}/local/$repo_name/cargo-target/$host"
		;;
	worktree | agent)
		target_dir="${artifact_root%/}/local/$repo_name/worktrees/$(cargo_target_key)/cargo-target/$host"
		;;
	*)
		printf 'unsupported CODEX_LAB_CARGO_TARGET_SCOPE=%q; expected shared, worktree, or agent\n' "$target_scope" >&2
		exit 2
		;;
	esac
	if [[ "$artifact_root_kind" == "path" ]]; then
		report_storage_route "using managed artifact path $artifact_root (volume identity not verified outside macOS)"
	elif [[ -n "$expected_volume_uuid" ]]; then
		report_storage_route "using managed artifact root $artifact_root (volume UUID $expected_volume_uuid)"
	else
		report_storage_route "using verified artifact volume $artifact_root (expected volume UUID not configured)"
	fi
else
	target_dir="$repo_root/codex-rs/target"
	report_storage_route "using portable repository target (artifact root unconfigured)"
fi

case "$target_dir" in
/*) ;;
*) target_dir="$repo_root/${target_dir#./}" ;;
esac

if [[ "${CODEX_LAB_CARGO_TARGET_NO_MKDIR:-}" != "1" ]]; then
	mkdir -p "$target_dir"
fi

printf '%s\n' "$target_dir"
