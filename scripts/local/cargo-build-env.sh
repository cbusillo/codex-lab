#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
repo_name="$(git -C "$repo_root" config --get remote.origin.url 2>/dev/null | sed -E 's#/*$##; s#\.git$##; s#^.*/##; s#^.*:##' || true)"
if [[ -z "$repo_name" ]]; then
	repo_name="codex-lab"
fi
artifact_root="${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-/Volumes/Developer-Artifacts}"

if [[ -n "${CODEX_LAB_CARGO_TARGET_DIR:-}" ]]; then
	target_dir="$CODEX_LAB_CARGO_TARGET_DIR"
elif [[ -d "$artifact_root" && -w "$artifact_root" ]]; then
	if command -v rustc >/dev/null 2>&1; then
		rustc_version="$(rustc -vV)"
		host="$(awk '/^host:/ { print $2; exit }' <<<"$rustc_version")"
	else
		host="$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"
	fi
	if [[ -z "$host" ]]; then
		host="unknown-host"
	fi
	target_dir="${artifact_root%/}/local/$repo_name/cargo-target/$host"
elif [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
	target_dir="$CARGO_TARGET_DIR"
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

printf 'export CARGO_TARGET_DIR=%q\n' "$target_dir"
