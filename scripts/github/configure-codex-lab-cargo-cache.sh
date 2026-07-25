#!/usr/bin/env bash
set -euo pipefail

workflow_leaf="${1:?usage: configure-codex-lab-cargo-cache.sh <workflow-cache-leaf> [bin-name] [cargo-profile]}"
bin_name="${2:-codex-lab}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
profile="${3:-release}"
cache_mode="default"

if [[ ! "$profile" =~ ^[[:alnum:]_-]+$ ]]; then
	echo "invalid Cargo profile: $profile" >&2
	exit 2
fi

host="unknown-host"
if command -v rustc >/dev/null 2>&1; then
	rustc_version="$(rustc -vV)"
	host="$(awk '/^host:/ { print $2; exit }' <<<"$rustc_version")"
fi
if [[ -z "$host" ]]; then
	host="unknown-host"
fi

if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
	target_dir="$CARGO_TARGET_DIR"
	cache_mode="preconfigured"
	echo "Using preconfigured Cargo target directory"
elif [[ "${RUNNER_ENVIRONMENT:-}" == "self-hosted" && -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" && -d "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT" ]]; then
	cache_root="${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT%/}/github-actions/cache/${GITHUB_REPOSITORY:?missing GITHUB_REPOSITORY}/$workflow_leaf"
	target_dir="${cache_root}/cargo-target-$host-$profile"
	mkdir -p "$target_dir"
	cache_mode="persistent"
	echo "Using configured persistent target cache"
else
	target_dir="$repo_root/codex-rs/target"
	echo "Using default Cargo target directory"
fi

profile_dir="$profile"
if [[ "$profile" == "dev" ]]; then
	profile_dir="debug"
fi
bin_path="$target_dir/$profile_dir/$bin_name"

{
	echo "CARGO_TARGET_DIR=$target_dir"
	echo "CODEX_LAB_BIN=$bin_path"
	echo "CODEX_LAB_CARGO_HOST=$host"
	echo "CODEX_LAB_CARGO_PROFILE=$profile"
	echo "CODEX_LAB_CARGO_CACHE_MODE=$cache_mode"
} >>"${GITHUB_ENV:?missing GITHUB_ENV}"

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
	{
		echo "### Codex Lab Cargo cache"
		echo
		echo "- workflow leaf: \`$workflow_leaf\`"
		echo "- cache mode: \`$cache_mode\`"
		echo "- host: \`$host\`"
		echo "- profile: \`$profile\`"
		echo "- target dir kind: \`absolute\`"
	} >>"$GITHUB_STEP_SUMMARY"
fi
