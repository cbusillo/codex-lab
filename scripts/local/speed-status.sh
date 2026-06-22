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
printf 'target_dir=%s\n' "$CARGO_TARGET_DIR"
printf 'target_size=%s\n' "$(human_size "$CARGO_TARGET_DIR")"
unset CARGO_TARGET_DIR

section "Exec Harness"
exec_harness_env="$(env -u CARGO_TARGET_DIR CODEX_EXEC_HARNESS_NO_MKDIR=1 "$repo_root/scripts/local/exec-harness-env.sh")"
eval "$exec_harness_env"
printf 'target_dir=%s\n' "$CARGO_TARGET_DIR"
printf 'target_size=%s\n' "$(human_size "$CARGO_TARGET_DIR")"
unset CARGO_TARGET_DIR

section "Artifact Root"
if [[ -n "${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-}" ]]; then
	printf 'root=%s\n' "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT"
	printf 'root_size=%s\n' "$(human_size "$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT")"
else
	printf 'root=not configured\n'
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
