#!/usr/bin/env bash
# Download and verify the Codex-built rusty_v8 release artifacts for TARGET.
#
# Upstream pinned these downloads to `openai/codex`, so a fork build silently
# linked V8 blobs published by another repository. Default to the repository
# running the build and require any other source to be requested explicitly, so
# the failure mode is "the release is missing" instead of "we linked someone
# else's binary".
set -euo pipefail

: "${TARGET:?TARGET environment variable is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP environment variable is required}"
: "${GITHUB_ENV:?GITHUB_ENV environment variable is required}"

repository="${RUSTY_V8_ARTIFACT_REPOSITORY:-${GITHUB_REPOSITORY:-}}"
server_url="${GITHUB_SERVER_URL:-https://github.com}"

if [[ -z "${repository}" ]]; then
  echo "No rusty_v8 artifact repository: set GITHUB_REPOSITORY or the action's artifact-repository input" >&2
  exit 1
fi

# Both values land in a URL, so reject anything that could add a path segment,
# a query string, or a second host.
if [[ ! "${repository}" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]]; then
  echo "Invalid rusty_v8 artifact repository: ${repository}" >&2
  exit 1
fi

if [[ ! "${server_url}" =~ ^https://[A-Za-z0-9.-]+(:[0-9]+)?/?$ ]]; then
  echo "Invalid GITHUB_SERVER_URL: ${server_url}" >&2
  exit 1
fi
server_url="${server_url%/}"

if [[ ! "${TARGET}" =~ ^[A-Za-z0-9_]+-[A-Za-z0-9_.-]+$ ]]; then
  echo "Invalid rusty_v8 target: ${TARGET}" >&2
  exit 1
fi

# Tests pin the version so they do not need a resolvable Bazel module graph.
version="${RUSTY_V8_CRATE_VERSION:-}"
if [[ -z "${version}" ]]; then
  : "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE environment variable is required}"
  version="$(python3 "${GITHUB_WORKSPACE}/.github/scripts/rusty_v8_bazel.py" resolved-v8-crate-version)"
fi

release_tag="rusty-v8-v${version}"
base_url="${server_url}/${repository}/releases/download/${release_tag}"
binding_dir="${RUNNER_TEMP}/rusty_v8"
archive_path="${binding_dir}/librusty_v8_release_${TARGET}.a.gz"
binding_path="${binding_dir}/src_binding_release_${TARGET}.rs"
checksums_path="${binding_dir}/rusty_v8_release_${TARGET}.sha256"

echo "Downloading rusty_v8 ${release_tag} artifacts for ${TARGET} from ${repository}"

mkdir -p "${binding_dir}"
curl_args=(
  --fail
  --show-error
  --location
  --retry 5
  --retry-all-errors
  --retry-delay 2
  --retry-max-time 120
)
curl "${curl_args[@]}" "${base_url}/librusty_v8_release_${TARGET}.a.gz" -o "${archive_path}"
curl "${curl_args[@]}" "${base_url}/src_binding_release_${TARGET}.rs" -o "${binding_path}"
curl "${curl_args[@]}" "${base_url}/rusty_v8_release_${TARGET}.sha256" -o "${checksums_path}"

if [[ "$(wc -l < "${checksums_path}")" -ne 2 ]]; then
  echo "Expected exactly two checksums for ${TARGET} in ${checksums_path}" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  (cd "${binding_dir}" && sha256sum -c "${checksums_path}")
else
  (cd "${binding_dir}" && shasum -a 256 -c "${checksums_path}")
fi

echo "RUSTY_V8_ARCHIVE=${archive_path}" >> "${GITHUB_ENV}"
echo "RUSTY_V8_SRC_BINDING_PATH=${binding_path}" >> "${GITHUB_ENV}"
