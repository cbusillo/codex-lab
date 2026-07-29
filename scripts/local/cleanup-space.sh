#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

apply=0
keep_exec_harness_cache=0
keep_local_cargo_cache=0
keep_artifact_temp=0

usage() {
	cat <<USAGE
Usage: scripts/local/cleanup-space.sh [--apply] [--keep-exec-harness-cache] [--keep-local-cargo-cache] [--keep-artifact-temp]

Remove rebuildable local build artifacts. The default mode is a dry run.

Options:
  --apply                   Delete paths instead of only listing them.
  --keep-exec-harness-cache Preserve the shared exec harness target cache.
  --keep-local-cargo-cache  Preserve the artifact-volume local Cargo target cache.
  --keep-artifact-temp      Preserve artifact-volume temporary output.
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
	--keep-artifact-temp)
		keep_artifact_temp=1
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

physical_path() {
	local path="$1"
	local dir
	local base

	if [[ -d "$path" && ! -L "$path" ]]; then
		(cd -P -- "$path" && pwd -P)
		return 0
	fi
	dir="$(dirname -- "$path")"
	base="$(basename -- "$path")"
	if [[ ! -d "$dir" ]]; then
		return 1
	fi
	printf '%s/%s\n' "$(cd -P -- "$dir" && pwd -P)" "$base"
}

is_bounded_exec_harness_output_root() {
	local path="$1"
	local physical
	if ! physical="$(physical_path "$path")"; then
		return 1
	fi
	local repo_output_root
	if repo_output_root="$(physical_path "$repo_root/.tmp/codex-exec-harness")"; then
		case "$physical" in
		"$repo_output_root" | "$repo_output_root"/*)
			return 0
			;;
		esac
	fi
	if [[ -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" ]]; then
		local artifact_root
		if ! artifact_root="$(physical_path "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT")"; then
			return 1
		fi
		case "$physical" in
		"$artifact_root"/local/*/exec-harness/output | "$artifact_root"/local/*/exec-harness/output/*)
			return 0
			;;
		esac
	fi
	return 1
}

is_bounded_repo_temp_path() {
	local path="$1"
	local physical
	if ! physical="$(physical_path "$path")"; then
		return 1
	fi
	local physical_repo_root
	physical_repo_root="$(cd -P -- "$repo_root" && pwd -P)"
	case "$physical" in
	"$physical_repo_root"/.tmp/*)
		return 0
		;;
	esac
	return 1
}

is_bounded_exec_harness_cache() {
	local path="$1"
	local physical
	if ! physical="$(physical_path "$path")"; then
		return 1
	fi
	if is_bounded_repo_temp_path "$path"; then
		return 0
	fi
	if [[ -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" ]]; then
		local artifact_root
		if ! artifact_root="$(physical_path "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT")"; then
			return 1
		fi
		case "$physical" in
		"$artifact_root"/local/*/exec-harness/cargo-target | "$artifact_root"/local/*/exec-harness/cargo-target/*)
			return 0
			;;
		esac
	fi

	local cache_home=""
	if [[ -n "${CODEX_LAB_HOME:-}" ]]; then
		cache_home="${CODEX_LAB_HOME%/}"
	elif [[ "$repo_root" == */.code/worktrees/*/* ]]; then
		cache_home="${repo_root%%/.code/worktrees/*}/.code"
	elif [[ "$repo_root" == */.code/working/*/branches/* ]]; then
		cache_home="${repo_root%%/.code/working/*}/.code"
	else
		cache_home="${HOME%/}/.codex-lab"
	fi
	local physical_cache_home
	if ! physical_cache_home="$(physical_path "$cache_home")"; then
		return 1
	fi
	case "$physical" in
	"$physical_cache_home"/working/_target-cache/*/exec-harness | "$physical_cache_home"/working/_target-cache/*/exec-harness/*)
		return 0
		;;
	esac
	return 1
}

is_bounded_local_cargo_cache() {
	local path="$1"
	local physical
	if ! physical="$(physical_path "$path")"; then
		return 1
	fi
	if is_bounded_repo_temp_path "$path"; then
		return 0
	fi
	local physical_repo_root
	physical_repo_root="$(cd -P -- "$repo_root" && pwd -P)"
	case "$physical" in
	"$physical_repo_root"/codex-rs/target | "$physical_repo_root"/target)
		return 0
		;;
	esac
	if [[ -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" ]]; then
		local artifact_root
		if ! artifact_root="$(physical_path "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT")"; then
			return 1
		fi
		case "$physical" in
		"$artifact_root"/local/*/cargo-target | "$artifact_root"/local/*/cargo-target/* | "$artifact_root"/local/*/worktrees/*/cargo-target | "$artifact_root"/local/*/worktrees/*/cargo-target/*)
			return 0
			;;
		esac
	fi
	return 1
}

is_bounded_artifact_temp_root() {
	local path="$1"
	local physical
	if ! physical="$(physical_path "$path")"; then
		return 1
	fi
	if [[ -z "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" ]]; then
		return 1
	fi
	local artifact_root
	if ! artifact_root="$(physical_path "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT")"; then
		return 1
	fi
	case "$physical" in
	"$artifact_root"/local/codex-lab/tmp | "$artifact_root"/local/codex-lab/tmp/*)
		return 0
		;;
	esac
	return 1
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

exec_harness_env="$(env -u CARGO_TARGET_DIR CODEX_EXEC_HARNESS_NO_MKDIR=1 "$repo_root/scripts/local/exec-harness-env.sh")"
eval "$exec_harness_env"
if [[ "$CODEX_EXEC_HARNESS_OUTPUT_ROOT" != "$repo_root/.tmp/codex-exec-harness" ]]; then
	remove_path "$repo_root/.tmp/codex-exec-harness" "legacy exec harness run artifacts"
fi
remove_path "$repo_root/.tmp/codex-exec-harness-ci" "exec harness CI artifacts"
if is_bounded_exec_harness_output_root "$CODEX_EXEC_HARNESS_OUTPUT_ROOT"; then
	remove_path "$CODEX_EXEC_HARNESS_OUTPUT_ROOT" "exec harness configured output root"
elif [[ -e "$CODEX_EXEC_HARNESS_OUTPUT_ROOT" || -L "$CODEX_EXEC_HARNESS_OUTPUT_ROOT" ]]; then
	printf '%8s  %s  (%s)\n' "skipped" "$CODEX_EXEC_HARNESS_OUTPUT_ROOT" "custom exec harness output root outside repo/artifact root"
fi
unset CODEX_EXEC_HARNESS_OUTPUT_ROOT
unset CODEX_EXEC_HARNESS_REPORT_JSON

if [[ "$keep_exec_harness_cache" -eq 0 ]]; then
	if is_bounded_exec_harness_cache "$CARGO_TARGET_DIR"; then
		remove_path "$CARGO_TARGET_DIR" "shared exec harness target cache"
	elif [[ -e "$CARGO_TARGET_DIR" || -L "$CARGO_TARGET_DIR" ]]; then
		printf '%8s  %s  (%s)\n' "skipped" "$CARGO_TARGET_DIR" "custom exec harness target cache outside bounded cache roots"
	fi
fi
unset CARGO_TARGET_DIR

if [[ "$keep_local_cargo_cache" -eq 0 ]]; then
	local_cargo_target="$(env -u CARGO_TARGET_DIR CODEX_LAB_CARGO_TARGET_NO_MKDIR=1 "$repo_root/scripts/local/cargo-build-env.sh")"
	if is_bounded_local_cargo_cache "$local_cargo_target"; then
		remove_path "$local_cargo_target" "artifact-volume local Cargo target cache"
	elif [[ -e "$local_cargo_target" || -L "$local_cargo_target" ]]; then
		printf '%8s  %s  (%s)\n' "skipped" "$local_cargo_target" "custom local Cargo target cache outside bounded cache roots"
	fi
fi

if [[ "$keep_artifact_temp" -eq 0 && -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" ]]; then
	artifact_tmpdir="${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT%/}/local/codex-lab/tmp"
	if is_bounded_artifact_temp_root "$artifact_tmpdir"; then
		remove_path "$artifact_tmpdir" "artifact-volume temporary output"
	elif [[ -e "$artifact_tmpdir" || -L "$artifact_tmpdir" ]]; then
		printf '%8s  %s  (%s)\n' "skipped" "$artifact_tmpdir" "artifact temp outside expected artifact root"
	fi
fi

echo
if [[ "$apply" -eq 1 ]]; then
	echo "Cleanup complete."
else
	echo "Dry run only. Re-run with --apply to delete these paths."
fi
