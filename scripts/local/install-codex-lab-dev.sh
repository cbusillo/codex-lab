#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bin_dir="${HOME}/.local/bin"
codex_lab_home="${HOME}/.codex-lab"
profile="dev"
force=0
marker="# codex-lab-dev-shim: managed by scripts/local/install-codex-lab-dev.sh"

usage() {
	cat <<USAGE
Usage: scripts/local/install-codex-lab-dev.sh [options]

Build and install a pinned PATH launcher for dogfooding Codex Lab from this checkout.
Requires Python 3.10 or newer.

Options:
  --bin-dir DIR            Directory where codex-lab should be installed.
                           Default: ~/.local/bin
  --codex-lab-home DIR     Default CODEX_LAB_HOME when the variable is unset.
                           Default: ~/.codex-lab
  --profile dev|release    Cargo build profile staged by the installer.
                           Default: dev
  --force                  Replace an existing non-managed codex-lab file.
  -h, --help               Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
	case "$1" in
	--bin-dir)
		if [[ $# -lt 2 ]]; then
			echo "error: --bin-dir requires a directory" >&2
			usage >&2
			exit 2
		fi
		bin_dir="$2"
		shift
		;;
	--codex-lab-home)
		if [[ $# -lt 2 ]]; then
			echo "error: --codex-lab-home requires a directory" >&2
			usage >&2
			exit 2
		fi
		codex_lab_home="$2"
		shift
		;;
	--profile)
		if [[ $# -lt 2 ]]; then
			echo "error: --profile requires dev or release" >&2
			usage >&2
			exit 2
		fi
		profile="$2"
		shift
		;;
	--force)
		force=1
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

case "$profile" in
dev)
	target_subdir="debug"
	;;
release)
	target_subdir="release"
	;;
*)
	echo "error: --profile must be dev or release" >&2
	exit 2
	;;
esac

python_bin="$(command -v python3 || true)"
if [[ -z "$python_bin" ]] || ! python_bin="$("$python_bin" -c 'import pathlib, sys; sys.exit(1) if sys.version_info < (3, 10) else print(pathlib.Path(sys.executable).resolve())')"; then
	echo "error: Python 3.10 or newer is required for the Codex Lab dev launcher" >&2
	exit 1
fi

mkdir -p "$bin_dir"
bin_dir="$(cd "$bin_dir" && pwd)"
mkdir -p "$codex_lab_home"
codex_lab_home="$(cd "$codex_lab_home" && pwd)"
shim_path="$bin_dir/codex-lab"

if [[ -e "$shim_path" || -L "$shim_path" ]]; then
	if [[ "$force" -ne 1 ]] && ! grep -Fq "$marker" "$shim_path" 2>/dev/null; then
		echo "error: refusing to replace non-managed launcher: $shim_path" >&2
		echo "Re-run with --force if this is intentional." >&2
		exit 1
	fi
fi

if ! command -v cargo >/dev/null 2>&1 && [[ -f "$HOME/.cargo/env" ]]; then
	# shellcheck disable=SC1091
	. "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
	echo "error: Cargo is required to build the Codex Lab dogfood candidate" >&2
	exit 1
fi

export CODEX_LAB_CARGO_TARGET_SCOPE="${CODEX_LAB_CARGO_TARGET_SCOPE:-worktree}"
CARGO_TARGET_DIR="$("$repo_root/scripts/local/cargo-build-env.sh")"
export CARGO_TARGET_DIR
(
	cd "$repo_root/codex-rs"
	cargo build -p codex-cli --bin codex-lab -p codex-code-mode-host --bin codex-code-mode-host --profile "$profile" --manifest-path Cargo.toml >/dev/null
)
target_root="${CARGO_TARGET_DIR:-$repo_root/codex-rs/target}"
candidate="$("$python_bin" "$repo_root/scripts/local/codex_lab_provenance.py" \
	--repo-root "$repo_root" \
	--binary "$target_root/$target_subdir/codex-lab" \
	--companion-binary "$target_root/$target_subdir/codex-code-mode-host" \
	--artifact-root "$codex_lab_home/working")"

tmp_path="$shim_path.tmp.$$"
trap 'rm -f -- "$tmp_path"' EXIT INT TERM
cat >"$tmp_path" <<EOF
#!/bin/sh
set -eu
$marker

DEFAULT_CODEX_LAB_HOME='$(printf "%s" "$codex_lab_home" | sed "s/'/'\\\\''/g")'
CANDIDATE='$(printf "%s" "$candidate" | sed "s/'/'\\\\''/g")'

export CODEX_LAB_HOME="\${CODEX_LAB_HOME:-\$DEFAULT_CODEX_LAB_HOME}"
mkdir -p "\$CODEX_LAB_HOME"

if [ ! -x "\$CANDIDATE" ]; then
  echo "error: pinned Codex Lab candidate is unavailable: \$CANDIDATE" >&2
  echo "Re-run scripts/local/install-codex-lab-dev.sh from the desired checkout." >&2
  exit 1
fi

exec "\$CANDIDATE" "\$@"
EOF
chmod 0755 "$tmp_path"
mv "$tmp_path" "$shim_path"
trap - EXIT INT TERM

echo "Installed Codex Lab dev launcher: $shim_path"
echo "Pinned dogfood candidate: $candidate"
echo "Default CODEX_LAB_HOME: $codex_lab_home"
case ":$PATH:" in
*":$bin_dir:"*) ;;
*) echo "Note: $bin_dir is not currently on PATH." ;;
esac
