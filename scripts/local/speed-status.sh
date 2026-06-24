#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

human_size() {
	local path="$1"
	if [[ -e "$path" || -L "$path" ]]; then
		du -sh "$path" 2>/dev/null | awk '{print $1}'
	else
		printf '0B'
	fi
}

section() {
	printf '\n%s\n' "$1"
}

section "Local Cargo"
local_cargo_env="$(env -u CARGO_TARGET_DIR CODEX_LAB_CARGO_TARGET_NO_MKDIR=1 "$repo_root/scripts/local/cargo-build-env.sh")"
eval "$local_cargo_env"
printf 'target_scope=%s\n' "${CODEX_LAB_CARGO_TARGET_SCOPE:-shared}"
printf 'target_dir=%s\n' "$CARGO_TARGET_DIR"
printf 'target_size=%s\n' "$(human_size "$CARGO_TARGET_DIR")"
unset CARGO_TARGET_DIR
worktree_cargo_env="$(env -u CARGO_TARGET_DIR -u CODEX_LAB_CARGO_TARGET_DIR CODEX_LAB_CARGO_TARGET_NO_MKDIR=1 CODEX_LAB_CARGO_TARGET_SCOPE=worktree "$repo_root/scripts/local/cargo-build-env.sh")"
eval "$worktree_cargo_env"
printf 'worktree_scoped_target_dir=%s\n' "$CARGO_TARGET_DIR"
printf 'worktree_scoped_target_size=%s\n' "$(human_size "$CARGO_TARGET_DIR")"
unset CARGO_TARGET_DIR
printf 'worktree_target=%s\n' "$repo_root/codex-rs/target"
printf 'worktree_target_size=%s\n' "$(human_size "$repo_root/codex-rs/target")"

section "Exec Harness"
exec_harness_env="$(env -u CARGO_TARGET_DIR CODEX_EXEC_HARNESS_NO_MKDIR=1 "$repo_root/scripts/local/exec-harness-env.sh")"
eval "$exec_harness_env"
printf 'target_dir=%s\n' "$CARGO_TARGET_DIR"
printf 'target_size=%s\n' "$(human_size "$CARGO_TARGET_DIR")"
unset CARGO_TARGET_DIR
printf 'output_root=%s\n' "$CODEX_EXEC_HARNESS_OUTPUT_ROOT"
printf 'output_size=%s\n' "$(human_size "$CODEX_EXEC_HARNESS_OUTPUT_ROOT")"
printf 'report_json=%s\n' "$CODEX_EXEC_HARNESS_REPORT_JSON"
unset CODEX_EXEC_HARNESS_OUTPUT_ROOT
unset CODEX_EXEC_HARNESS_REPORT_JSON

section "Artifact Root"
if [[ -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" ]]; then
	printf 'root=%s\n' "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT"
	printf 'root_size=%s\n' "$(human_size "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT")"
else
	printf 'root=not configured\n'
fi

section "Bazel"
if [[ -f "$repo_root/user.bazelrc" ]]; then
	while IFS= read -r line; do
		case "$line" in
		common\ --disk_cache=* | common\ --repo_contents_cache=* | common\ --repository_cache=* | common\ --output_base=* | common\ --output_user_root=*)
			name="${line#common --}"
			name="${name%%=*}"
			path="${line#*=}"
			printf '%s=%s\n' "$name" "$path"
			printf '%s_size=%s\n' "$name" "$(human_size "$path")"
			;;
		esac
	done <"$repo_root/user.bazelrc"
else
	printf 'user_bazelrc=not configured\n'
fi

section "Node"
if [[ -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" ]]; then
	if artifact_env="$("$repo_root/scripts/local/artifact-env.sh" --no-mkdir 2>/dev/null)"; then
		artifact_pnpm_store="$(
			eval "$artifact_env"
			printf '%s' "$pnpm_config_store_dir"
		)"
		artifact_npm_cache="$(
			eval "$artifact_env"
			printf '%s' "$NPM_CONFIG_CACHE"
		)"
		artifact_tmpdir="$(
			eval "$artifact_env"
			printf '%s' "$TMPDIR"
		)"
		artifact_sccache_dir="$(
			eval "$artifact_env"
			printf '%s' "$SCCACHE_DIR"
		)"
		printf 'artifact_pnpm_store=%s\n' "$artifact_pnpm_store"
		printf 'artifact_pnpm_store_size=%s\n' "$(human_size "$artifact_pnpm_store")"
		printf 'artifact_npm_cache=%s\n' "$artifact_npm_cache"
		printf 'artifact_npm_cache_size=%s\n' "$(human_size "$artifact_npm_cache")"
	else
		printf 'artifact_node_cache=artifact root unavailable\n'
		artifact_tmpdir=""
		artifact_sccache_dir=""
	fi
else
	printf 'artifact_node_cache=artifact root not configured\n'
	artifact_tmpdir=""
	artifact_sccache_dir=""
fi
if command -v pnpm >/dev/null 2>&1; then
	pnpm_store="$(pnpm store path 2>/dev/null || true)"
	if [[ -n "$pnpm_store" ]]; then
		printf 'pnpm_store=%s\n' "$pnpm_store"
		printf 'pnpm_store_size=%s\n' "$(human_size "$pnpm_store")"
	else
		printf 'pnpm_store=unavailable\n'
	fi
else
	printf 'pnpm_store=pnpm not found\n'
fi
if command -v npm >/dev/null 2>&1; then
	npm_cache="$(npm config get cache 2>/dev/null || true)"
	if [[ -n "$npm_cache" ]]; then
		printf 'npm_cache=%s\n' "$npm_cache"
		printf 'npm_cache_size=%s\n' "$(human_size "$npm_cache")"
	else
		printf 'npm_cache=unavailable\n'
	fi
else
	printf 'npm_cache=npm not found\n'
fi

section "Sccache"
if command -v sccache >/dev/null 2>&1; then
	printf 'binary=%s\n' "$(command -v sccache)"
	printf 'rustc_wrapper=%s\n' "${RUSTC_WRAPPER:-not configured}"
	printf 'cache_dir=%s\n' "${SCCACHE_DIR:-not configured}"
	if [[ -n "$artifact_sccache_dir" ]]; then
		printf 'artifact_cache_dir=%s\n' "$artifact_sccache_dir"
		printf 'artifact_cache_size=%s\n' "$(human_size "$artifact_sccache_dir")"
	fi
	if [[ -n "${SCCACHE_DIR:-}" ]]; then
		printf 'cache_size=%s\n' "$(human_size "$SCCACHE_DIR")"
	fi
else
	printf 'binary=not found\n'
	if [[ -n "$artifact_sccache_dir" ]]; then
		printf 'artifact_cache_dir=%s\n' "$artifact_sccache_dir"
		printf 'artifact_cache_size=%s\n' "$(human_size "$artifact_sccache_dir")"
	fi
fi

section "Temp"
printf 'repo_tmp=%s\n' "$repo_root/.tmp"
printf 'repo_tmp_size=%s\n' "$(human_size "$repo_root/.tmp")"
if [[ -n "$artifact_tmpdir" ]]; then
	printf 'artifact_tmpdir=%s\n' "$artifact_tmpdir"
	printf 'artifact_tmpdir_size=%s\n' "$(human_size "$artifact_tmpdir")"
fi
if [[ -n "${TMPDIR:-}" ]]; then
	printf 'tmpdir=%s\n' "$TMPDIR"
fi

section "GitHub Runners"
if command -v gh >/dev/null 2>&1; then
	if gh repo view >/dev/null 2>&1; then
		gh api "repos/{owner}/{repo}/actions/runners" --paginate \
			--jq '.runners[] | [.name, .os, .status, (.busy|tostring), ([.labels[].name] | join(","))] | @tsv' \
			2>/dev/null || printf 'unavailable\n'
	else
		printf 'unavailable: gh repo context not available\n'
	fi
else
	printf 'unavailable: gh not found\n'
fi

section "Remote Compile Host"
if [[ -n "${CODEX_LAB_REMOTE_COMPILE_HOST:-}" ]]; then
	ssh -o BatchMode=yes -o ConnectTimeout=5 "$CODEX_LAB_REMOTE_COMPILE_HOST" \
		'printf "host=%s\n" "$(hostname)"; command -v cargo || true; command -v rustc || true; cargo --version 2>/dev/null || true; rustc --version 2>/dev/null || true' \
		2>/dev/null || printf 'unavailable\n'
else
	printf 'host=not configured\n'
fi
