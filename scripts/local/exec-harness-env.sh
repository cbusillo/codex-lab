#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
repo_name="$(git -C "$repo_root" config --get remote.origin.url 2>/dev/null | sed -E 's#/*$##; s#\.git$##; s#^.*/##; s#^.*:##' || true)"
if [[ -z "$repo_name" ]]; then
	repo_leaf="$(basename "$repo_root")"
	repo_slug="$(printf '%s' "$repo_leaf" | tr -c '[:alnum:]._+-' '-')"
	repo_hash="$(printf '%s' "$repo_root" | shasum -a 256 | awk '{ print substr($1, 1, 12) }')"
	repo_name="${repo_slug:-workspace}-$repo_hash"
fi
artifact_root="${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}"
derived_cache_home=""
if [[ "$repo_root" == */.code/worktrees/*/* ]]; then
	derived_cache_home="${repo_root%%/.code/worktrees/*}/.code"
elif [[ "$repo_root" == */.code/working/*/branches/* ]]; then
	derived_cache_home="${repo_root%%/.code/working/*}/.code"
fi

if [[ -n "${CODEX_EXEC_HARNESS_CARGO_TARGET_DIR:-}" ]]; then
	target_dir="$CODEX_EXEC_HARNESS_CARGO_TARGET_DIR"
elif [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
	target_dir="$CARGO_TARGET_DIR"
elif [[ -n "$artifact_root" && -d "$artifact_root" && -w "$artifact_root" ]]; then
	if command -v rustc >/dev/null 2>&1; then
		rustc_version="$(rustc -vV)"
		host="$(awk '/^host:/ { print $2; exit }' <<<"$rustc_version")"
	else
		host="$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"
	fi
	if [[ -z "$host" ]]; then
		host="unknown-host"
	fi
	target_dir="${artifact_root%/}/local/$repo_name/exec-harness/cargo-target/$host"
elif [[ -n "${CODEX_LAB_HOME:-}" ]]; then
	target_dir="${CODEX_LAB_HOME%/}/working/_target-cache/$repo_name/exec-harness"
elif [[ -n "$derived_cache_home" ]]; then
	target_dir="$derived_cache_home/working/_target-cache/$repo_name/exec-harness"
else
	target_dir="${HOME%/}/.codex-lab/working/_target-cache/$repo_name/exec-harness"
fi

case "$target_dir" in
/*) ;;
*) target_dir="$repo_root/${target_dir#./}" ;;
esac

if [[ "${CODEX_EXEC_HARNESS_NO_MKDIR:-}" != "1" ]]; then
	mkdir -p "$target_dir"
fi
printf 'export CARGO_TARGET_DIR=%q\n' "$target_dir"
