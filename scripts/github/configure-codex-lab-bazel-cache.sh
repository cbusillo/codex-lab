#!/usr/bin/env bash
set -euo pipefail

workflow_leaf="${1:?usage: configure-codex-lab-bazel-cache.sh <workflow-cache-leaf> [disk-cache-max-size]}"
disk_cache_max_size="${2:-100G}"
cache_mode="ephemeral"

if [[ ! "$workflow_leaf" =~ ^[[:alnum:]][[:alnum:]_.-]*$ ]]; then
	echo "invalid workflow cache leaf: $workflow_leaf" >&2
	exit 2
fi
if [[ ! "$disk_cache_max_size" =~ ^[1-9][0-9]*[KMGT]?$ ]]; then
	echo "invalid Bazel disk cache maximum size: $disk_cache_max_size" >&2
	exit 2
fi

if [[ "${RUNNER_ENVIRONMENT:-}" == "self-hosted" && -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" && -d "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT" ]]; then
	github_repository="${GITHUB_REPOSITORY:?missing GITHUB_REPOSITORY}"
	if [[ ! "$github_repository" =~ ^[[:alnum:]_.-]+/[[:alnum:]_.-]+$ ]]; then
		echo "invalid GitHub repository: $github_repository" >&2
		exit 2
	fi

	cache_root="${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT%/}/github-actions/cache/$github_repository/$workflow_leaf"
	disk_cache="$cache_root/bazel-disk-cache"
	repository_cache="$cache_root/bazel-repository-cache"
	mkdir -p "$disk_cache" "$repository_cache"
	cache_mode="persistent"

	{
		echo "BAZEL_DISK_CACHE=$disk_cache"
		echo "BAZEL_DISK_CACHE_GC_MAX_SIZE=$disk_cache_max_size"
		echo "BAZEL_REPOSITORY_CACHE=$repository_cache"
	} >>"${GITHUB_ENV:?missing GITHUB_ENV}"

	echo "Using configured persistent Bazel caches"
else
	echo "Using setup-bazel-ci ephemeral caches"
fi

echo "CODEX_LAB_BAZEL_CACHE_MODE=$cache_mode" >>"${GITHUB_ENV:?missing GITHUB_ENV}"

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
	{
		echo "### Codex Lab Bazel cache"
		echo
		echo "- workflow leaf: \`$workflow_leaf\`"
		echo "- cache mode: \`$cache_mode\`"
		echo "- disk cache limit: \`$disk_cache_max_size\`"
	} >>"$GITHUB_STEP_SUMMARY"
fi
