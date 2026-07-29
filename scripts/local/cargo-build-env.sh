#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
repo_name="$(git -C "$repo_root" config --get remote.origin.url 2>/dev/null | sed -E 's#/*$##; s#\.git$##; s#^.*/##; s#^.*:##' || true)"
if [[ -z "$repo_name" ]]; then
	repo_name="codex-lab"
fi
artifact_root="${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}"

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
elif [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
	if [[ -n "${CODEX_LAB_CARGO_TARGET_SCOPE:-}" && "${CODEX_LAB_CARGO_TARGET_SCOPE:-}" != "shared" ]]; then
		printf 'warning: CARGO_TARGET_DIR is already set; ignoring CODEX_LAB_CARGO_TARGET_SCOPE=%q\n' "$CODEX_LAB_CARGO_TARGET_SCOPE" >&2
	fi
	target_dir="$CARGO_TARGET_DIR"
elif [[ -n "$artifact_root" && -d "$artifact_root" && -w "$artifact_root" ]]; then
	host="$(host_triple)"
	target_scope="${CODEX_LAB_CARGO_TARGET_SCOPE:-shared}"
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
else
	target_dir="$repo_root/codex-rs/target"
fi

case "$target_dir" in
/*) ;;
*) target_dir="$repo_root/${target_dir#./}" ;;
esac

if [[ "${CODEX_LAB_CARGO_TARGET_NO_MKDIR:-}" != "1" ]]; then
	mkdir -p "$target_dir"
fi

printf '%s\n' "$target_dir"
