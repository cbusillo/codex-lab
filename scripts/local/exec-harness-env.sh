#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
repo_name="$(basename "$repo_root")"
derived_cache_home=""
if [[ "$repo_root" == */.code/worktrees/*/* ]]; then
	repo_name="$(basename "${repo_root%/*}")"
	derived_cache_home="${repo_root%%/.code/worktrees/*}/.code"
elif [[ "$repo_root" == */.code/working/*/branches/* ]]; then
	repo_name="$(basename "${repo_root%/branches/*}")"
	derived_cache_home="${repo_root%%/.code/working/*}/.code"
fi

if [[ -n "${CODEX_EXEC_HARNESS_CARGO_TARGET_DIR:-}" ]]; then
	target_dir="$CODEX_EXEC_HARNESS_CARGO_TARGET_DIR"
elif [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
	target_dir="$CARGO_TARGET_DIR"
elif [[ -n "${CODE_HOME:-}" ]]; then
	target_dir="${CODE_HOME%/}/working/_target-cache/$repo_name/exec-harness"
elif [[ -n "${CODEX_HOME:-}" ]]; then
	target_dir="${CODEX_HOME%/}/working/_target-cache/$repo_name/exec-harness"
elif [[ -n "$derived_cache_home" ]]; then
	target_dir="$derived_cache_home/working/_target-cache/$repo_name/exec-harness"
elif [[ -d "$HOME/.code" ]]; then
	target_dir="$HOME/.code/working/_target-cache/$repo_name/exec-harness"
elif [[ -d "$HOME/.codex" ]]; then
	target_dir="$HOME/.codex/working/_target-cache/$repo_name/exec-harness"
else
	target_dir="$repo_root/.code/working/_target-cache/$repo_name/exec-harness"
fi

case "$target_dir" in
/*) ;;
*) target_dir="$repo_root/${target_dir#./}" ;;
esac

if [[ "${CODEX_EXEC_HARNESS_NO_MKDIR:-}" != "1" ]]; then
	mkdir -p "$target_dir"
fi
printf 'export CARGO_TARGET_DIR=%q\n' "$target_dir"
