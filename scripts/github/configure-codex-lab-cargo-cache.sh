#!/usr/bin/env bash
set -euo pipefail

workflow_leaf="${1:?usage: configure-codex-lab-cargo-cache.sh <workflow-cache-leaf>}"
bin_name="${2:-codex-lab}"

if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
	target_dir="$CARGO_TARGET_DIR"
	echo "Using preconfigured Cargo target directory: $target_dir"
elif [[ "${RUNNER_ENVIRONMENT:-}" == "self-hosted" && -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" && -d "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT" ]]; then
	cache_root="${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT%/}/github-actions/cache/${GITHUB_REPOSITORY:?missing GITHUB_REPOSITORY}/$workflow_leaf"
	target_dir="${cache_root}/cargo-target-aarch64-apple-darwin-release"
	mkdir -p "$target_dir"
	echo "Using configured persistent target cache: $target_dir"
else
	target_dir="codex-rs/target"
	echo "Using default Cargo target directory"
fi

case "$target_dir" in
/*) bin_path="$target_dir/release/$bin_name" ;;
*) bin_path="$target_dir/release/$bin_name" ;;
esac

{
	echo "CARGO_TARGET_DIR=$target_dir"
	echo "CODEX_LAB_BIN=$bin_path"
} >>"${GITHUB_ENV:?missing GITHUB_ENV}"
