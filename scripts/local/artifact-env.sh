#!/usr/bin/env bash
set -euo pipefail

artifact_root="${CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT:-/Volumes/Developer-Artifacts}"
enable_sccache=0
no_mkdir=0

usage() {
	cat <<USAGE
Usage: scripts/local/artifact-env.sh [--artifact-root DIR] [--enable-sccache] [--no-mkdir]

Print sourceable exports for local package-manager caches, temp output, and
optional sccache routing under the artifact root.

Options:
  --artifact-root DIR  Artifact volume root. Default: CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT
                       or /Volumes/Developer-Artifacts.
  --enable-sccache     Export RUSTC_WRAPPER when sccache is installed.
  --no-mkdir           Do not create directories; useful for status/cleanup probes.
  -h, --help           Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
	case "$1" in
	--artifact-root)
		if [[ $# -lt 2 ]]; then
			echo "error: --artifact-root requires a directory" >&2
			usage >&2
			exit 2
		fi
		artifact_root="$2"
		shift
		;;
	--enable-sccache)
		enable_sccache=1
		;;
	--no-mkdir)
		no_mkdir=1
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

case "$artifact_root" in
/*) ;;
*)
	echo "error: artifact root must be an absolute path: $artifact_root" >&2
	exit 2
	;;
esac

artifact_root="${artifact_root%/}"
if [[ ! -d "$artifact_root" || ! -w "$artifact_root" ]]; then
	echo "error: artifact root is not mounted or writable: $artifact_root" >&2
	exit 1
fi

local_root="$artifact_root/local/codex-lab"
pnpm_store="$local_root/pnpm-store"
npm_cache="$local_root/npm-cache"
tmp_root="$local_root/tmp"
sccache_dir="$local_root/sccache"

if [[ "$no_mkdir" -ne 1 ]]; then
	mkdir -p "$pnpm_store" "$npm_cache" "$tmp_root"
	if [[ "$enable_sccache" -eq 1 ]]; then
		mkdir -p "$sccache_dir"
	fi
fi

printf 'export pnpm_config_store_dir=%q\n' "$pnpm_store"
printf 'export NPM_CONFIG_CACHE=%q\n' "$npm_cache"
printf 'export TMPDIR=%q\n' "$tmp_root"
printf 'export TMP=%q\n' "$tmp_root"
printf 'export TEMP=%q\n' "$tmp_root"
printf 'export SCCACHE_DIR=%q\n' "$sccache_dir"
if [[ "$enable_sccache" -eq 1 ]]; then
	if sccache_bin="$(command -v sccache 2>/dev/null)"; then
		printf 'export RUSTC_WRAPPER=%q\n' "$sccache_bin"
	else
		echo "warning: --enable-sccache requested but sccache was not found" >&2
	fi
fi
