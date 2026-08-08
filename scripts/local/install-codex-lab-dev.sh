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
candidate_dirty_state="$(basename "$(dirname "$(dirname "$candidate")")")"
candidate_commit="$(basename "$(dirname "$(dirname "$(dirname "$candidate")")")")"
source_common_dir="$(git -C "$repo_root" rev-parse --git-common-dir)"
case "$source_common_dir" in
/*) ;;
*) source_common_dir="$repo_root/$source_common_dir" ;;
esac
source_repository_id="$(cd "$source_common_dir" && pwd -P)"

tmp_path="$shim_path.tmp.$$"
trap 'rm -f -- "$tmp_path"' EXIT INT TERM
cat >"$tmp_path" <<EOF
#!/bin/sh
set -eu
$marker

DEFAULT_CODEX_LAB_HOME='$(printf "%s" "$codex_lab_home" | sed "s/'/'\\\\''/g")'
CANDIDATE='$(printf "%s" "$candidate" | sed "s/'/'\\\\''/g")'
CANDIDATE_COMMIT='$(printf "%s" "$candidate_commit" | sed "s/'/'\\\\''/g")'
CANDIDATE_DIRTY_STATE='$(printf "%s" "$candidate_dirty_state" | sed "s/'/'\\\\''/g")'
SOURCE_CHECKOUT='$(printf "%s" "$repo_root" | sed "s/'/'\\\\''/g")'
SOURCE_REPOSITORY_ID='$(printf "%s" "$source_repository_id" | sed "s/'/'\\\\''/g")'
INSTALL_BIN_DIR='$(printf "%s" "$bin_dir" | sed "s/'/'\\\\''/g")'
INSTALL_PROFILE='$(printf "%s" "$profile" | sed "s/'/'\\\\''/g")'

export CODEX_LAB_HOME="\${CODEX_LAB_HOME:-\$DEFAULT_CODEX_LAB_HOME}"
mkdir -p "\$CODEX_LAB_HOME"

unset CODEX_LAB_PINNED_CANDIDATE_WARNING

shell_quote() {
  printf "'"
  printf '%s' "\$1" | sed "s/'/'\"'\"'/g"
  printf "'"
}

same_repository_checkout() {
  checkout="\$1"
  checkout_root="\$(git -C "\$checkout" rev-parse --show-toplevel 2>/dev/null)" || return 1
  common_dir="\$(git -C "\$checkout_root" rev-parse --git-common-dir 2>/dev/null)" || return 1
  case "\$common_dir" in
    /*) ;;
    *) common_dir="\$checkout_root/\$common_dir" ;;
  esac
  common_dir="\$(cd "\$common_dir" 2>/dev/null && pwd -P)" || return 1
  [ "\$common_dir" = "\$SOURCE_REPOSITORY_ID" ] || return 1
  EVIDENCE_CHECKOUT="\$checkout_root"
}

evidence_argument=''
expect_evidence_argument=0
for argument in "\$@"; do
  if [ "\$expect_evidence_argument" -eq 1 ]; then
    evidence_argument="\$argument"
    expect_evidence_argument=0
    continue
  fi
  case "\$argument" in
    --) break ;;
    -C|--cd) expect_evidence_argument=1 ;;
    --cd=*) evidence_argument="\${argument#--cd=}" ;;
  esac
done

EVIDENCE_CHECKOUT=''
evidence_state='git-missing'
if command -v git >/dev/null 2>&1; then
  evidence_state='checkout-unavailable'
  if [ -n "\$evidence_argument" ]; then
    same_repository_checkout "\$evidence_argument" || true
  fi
  if [ -z "\$EVIDENCE_CHECKOUT" ]; then
    same_repository_checkout "\$PWD" || true
  fi
  if [ -z "\$EVIDENCE_CHECKOUT" ]; then
    same_repository_checkout "\$SOURCE_CHECKOUT" || true
  fi
  if [ -n "\$EVIDENCE_CHECKOUT" ]; then
    evidence_state='inspect'
  elif [ ! -d "\$SOURCE_CHECKOUT" ]; then
    evidence_state='source-checkout-missing'
  fi
fi

printf 'Pinned Codex Lab candidate: %s (%s)\n' "\$CANDIDATE_COMMIT" "\$CANDIDATE_DIRTY_STATE" >&2

if [ "\$evidence_state" = inspect ]; then
  evidence_commit="\$(git -C "\$EVIDENCE_CHECKOUT" rev-parse --verify HEAD 2>/dev/null || true)"
  evidence_dirty_state=unavailable
  if evidence_status="\$(git -C "\$EVIDENCE_CHECKOUT" status --porcelain --untracked-files=no 2>/dev/null)"; then
    evidence_dirty_state=clean
    if [ -n "\$evidence_status" ]; then
      evidence_dirty_state=dirty
    fi
  fi

  installer="\$EVIDENCE_CHECKOUT/scripts/local/install-codex-lab-dev.sh"
  if [ -z "\$evidence_commit" ] || [ "\$evidence_dirty_state" = unavailable ]; then
    evidence_state='unreadable'
  elif [ "\$evidence_dirty_state" = dirty ]; then
    evidence_state='dirty'
  elif [ "\$evidence_commit" = "\$CANDIDATE_COMMIT" ]; then
    evidence_state='current'
  elif git -C "\$EVIDENCE_CHECKOUT" cat-file -e "\$CANDIDATE_COMMIT^{commit}" 2>/dev/null; then
    if git -C "\$EVIDENCE_CHECKOUT" merge-base --is-ancestor "\$CANDIDATE_COMMIT" "\$evidence_commit" 2>/dev/null; then
      if [ ! -e "\$installer" ]; then
        evidence_state='installer-missing'
      elif [ ! -x "\$installer" ]; then
        evidence_state='installer-not-executable'
      else
        evidence_state='source-newer'
      fi
    elif git -C "\$EVIDENCE_CHECKOUT" merge-base --is-ancestor "\$evidence_commit" "\$CANDIDATE_COMMIT" 2>/dev/null; then
      evidence_state='candidate-newer'
    else
      evidence_state='diverged'
    fi
  else
    evidence_state='incomparable'
  fi
fi

case "\$evidence_state" in
current)
  ;;
source-newer)
  refresh_command="\$(shell_quote "\$installer") --bin-dir \$(shell_quote "\$INSTALL_BIN_DIR") --codex-lab-home \$(shell_quote "\$DEFAULT_CODEX_LAB_HOME") --profile \$(shell_quote "\$INSTALL_PROFILE")"
  CODEX_LAB_PINNED_CANDIDATE_WARNING="Pinned Codex Lab candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) is older than clean local source \$evidence_commit at \$EVIDENCE_CHECKOUT. Refresh command: \$refresh_command"
  ;;
candidate-newer)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="Pinned Codex Lab candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) is ahead of clean local source \$evidence_commit at \$EVIDENCE_CHECKOUT. Reinstalling from this checkout would downgrade the candidate, so no command is provided. Select a clean checkout at the candidate commit or newer."
  ;;
dirty)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="Local source \$evidence_commit is dirty at \$EVIDENCE_CHECKOUT; candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) remains pinned. Commit or stash the changes, or select a clean worktree, before reinstalling."
  ;;
diverged)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="Pinned Codex Lab candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) and clean local source \$evidence_commit at \$EVIDENCE_CHECKOUT have diverged. Select a clean checkout from the intended history before reinstalling; no replacement command is provided."
  ;;
incomparable)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="Pinned Codex Lab candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) cannot be compared with clean local source \$evidence_commit at \$EVIDENCE_CHECKOUT. Select a clean checkout containing the intended candidate history before reinstalling."
  ;;
installer-missing)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="Pinned Codex Lab candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) is older than clean local source \$evidence_commit at \$EVIDENCE_CHECKOUT, but that checkout does not contain scripts/local/install-codex-lab-dev.sh. Use a clean intended checkout with the supported installer; no command is provided."
  ;;
installer-not-executable)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="Pinned Codex Lab candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) is older than clean local source \$evidence_commit at \$EVIDENCE_CHECKOUT, but scripts/local/install-codex-lab-dev.sh is not executable there. Restore an executable supported installer in a clean checkout; no command is provided."
  ;;
unreadable)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="Local source evidence at \$EVIDENCE_CHECKOUT is not stageable because its commit or clean status could not be read. Candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) remains pinned; repair Git metadata or select a clean worktree before reinstalling."
  ;;
git-missing)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="Local source freshness is unavailable because Git is not on PATH. Candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) remains pinned; install Git and launch from a clean intended checkout to compare provenance."
  ;;
source-checkout-missing)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="The recorded source checkout is unavailable at \$SOURCE_CHECKOUT. Candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) remains pinned; launch from a clean checkout of the same repository to compare provenance."
  ;;
checkout-unavailable)
  CODEX_LAB_PINNED_CANDIDATE_WARNING="No same-repository source checkout is available for provenance comparison. Candidate \$CANDIDATE_COMMIT (\$CANDIDATE_DIRTY_STATE) remains pinned; launch from a clean checkout of the intended repository."
  ;;
esac

if [ -n "\${CODEX_LAB_PINNED_CANDIDATE_WARNING:-}" ]; then
  export CODEX_LAB_PINNED_CANDIDATE_WARNING
  printf 'warning: %s\n' "\$CODEX_LAB_PINNED_CANDIDATE_WARNING" >&2
fi

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
