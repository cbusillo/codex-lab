#!/usr/bin/env bash
# Install apt packages only when they are actually missing, and only when this
# runner can escalate without a password prompt.
#
# The Linux release/musl jobs run on persistent self-hosted runners as well as
# ephemeral GitHub-hosted ones. Unconditional `sudo apt-get install` hangs on a
# password prompt where sudo is not passwordless, and rewrites the package set
# of a persistent runner on every build. Follow the setup-ci pattern: do the
# privileged thing when it is available and free, otherwise fail with the exact
# packages an operator must preinstall.
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  echo "usage: install-apt-packages.sh <package>..." >&2
  exit 1
fi

apt_update_args=()
if [[ -n "${APT_UPDATE_ARGS:-}" ]]; then
  # shellcheck disable=SC2206
  apt_update_args=(${APT_UPDATE_ARGS})
fi

apt_install_args=()
if [[ -n "${APT_INSTALL_ARGS:-}" ]]; then
  # shellcheck disable=SC2206
  apt_install_args=(${APT_INSTALL_ARGS})
fi

missing=()
for package in "$@"; do
  status="$(dpkg-query -W -f='${db:Status-Status}' "${package}" 2>/dev/null || true)"
  if [[ "${status}" != "installed" ]]; then
    missing+=("${package}")
  fi
done

if [[ "${#missing[@]}" -eq 0 ]]; then
  echo "All apt packages already installed: $*"
  exit 0
fi

if [[ "$(id -u)" -eq 0 ]]; then
  run_privileged() { env "$@"; }
elif sudo -n true >/dev/null 2>&1; then
  run_privileged() { sudo env "$@"; }
else
  echo "Cannot install missing apt packages without passwordless sudo: ${missing[*]}" >&2
  echo "Preinstall them on this runner, or grant the runner passwordless sudo." >&2
  exit 1
fi

echo "Installing missing apt packages: ${missing[*]}"
if [[ "${#apt_update_args[@]}" -gt 0 ]]; then
  run_privileged DEBIAN_FRONTEND=noninteractive apt-get update "${apt_update_args[@]}"
else
  run_privileged DEBIAN_FRONTEND=noninteractive apt-get update
fi

if [[ "${#apt_install_args[@]}" -gt 0 ]]; then
  run_privileged DEBIAN_FRONTEND=noninteractive apt-get install -y \
    "${apt_install_args[@]}" "${missing[@]}"
else
  run_privileged DEBIAN_FRONTEND=noninteractive apt-get install -y "${missing[@]}"
fi
