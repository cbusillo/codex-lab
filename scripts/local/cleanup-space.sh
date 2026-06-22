#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

apply=0
keep_exec_harness_cache=0
keep_local_cargo_cache=0

usage() {
	cat <<USAGE
Usage: scripts/local/cleanup-space.sh [--apply] [--keep-exec-harness-cache] [--keep-local-cargo-cache]

Remove rebuildable local build artifacts. The default mode is a dry run.

Options:
  --apply                   Delete paths instead of only listing them.
  --keep-exec-harness-cache Preserve the shared exec harness target cache.
  --keep-local-cargo-cache  Preserve the artifact-volume local Cargo target cache.
  -h, --help                Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
	case "$1" in
	--apply)
		apply=1
		;;
	--keep-exec-harness-cache)
		keep_exec_harness_cache=1
		;;
	--keep-local-cargo-cache)
		keep_local_cargo_cache=1
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "error: unknown option: $1" >&2
		usage >&2
		exit 2
		;;
	esac
	shift
done

human_size() {
	local path="$1"
	if [[ -e "$path" || -L "$path" ]]; then
		du -sh "$path" 2>/dev/null | awk '{print $1}'
	else
		printf '0B'
	fi
}

remove_path() {
	local path="$1"
	local note="${2:-}"

	if [[ ! -e "$path" && ! -L "$path" ]]; then
		return 0
	fi

	local size
	size="$(human_size "$path")"
	if [[ -n "$note" ]]; then
		printf '%8s  %s  (%s)\n' "$size" "$path" "$note"
	else
		printf '%8s  %s\n' "$size" "$path"
	fi

	if [[ "$apply" -eq 1 ]]; then
		rm -rf -- "$path"
	fi
}

mode="dry run"
if [[ "$apply" -eq 1 ]]; then
	mode="apply"
fi

echo "Local space cleanup ($mode)"
echo "Repo: $repo_root"
echo
echo "Cleanup candidates:"

remove_path "$repo_root/codex-rs/target" "worktree Cargo target"
remove_path "$repo_root/target" "legacy/root Cargo target"
remove_path "$repo_root/.tmp/codex-exec-harness" "exec harness run artifacts"
remove_path "$repo_root/.tmp/codex-exec-harness-ci" "exec harness CI artifacts"

if [[ "$keep_exec_harness_cache" -eq 0 ]]; then
	exec_harness_env="$(CODEX_EXEC_HARNESS_NO_MKDIR=1 "$repo_root/scripts/local/exec-harness-env.sh")"
	eval "$exec_harness_env"
	remove_path "$CARGO_TARGET_DIR" "shared exec harness target cache"
fi

if [[ "$keep_local_cargo_cache" -eq 0 ]]; then
	local_cargo_env="$(CODEX_LAB_CARGO_TARGET_NO_MKDIR=1 "$repo_root/scripts/local/cargo-build-env.sh")"
	eval "$local_cargo_env"
	remove_path "$CARGO_TARGET_DIR" "artifact-volume local Cargo target cache"
fi

echo
if [[ "$apply" -eq 1 ]]; then
	echo "Cleanup complete."
else
	echo "Dry run only. Re-run with --apply to delete these paths."
fi
