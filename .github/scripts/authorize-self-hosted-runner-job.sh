#!/usr/bin/env bash
set -euo pipefail

readonly expected_repository="cbusillo/codex-lab"
readonly repository="${GITHUB_REPOSITORY:-}"
readonly actor="${GITHUB_ACTOR:-}"
readonly triggering_actor="${GITHUB_TRIGGERING_ACTOR:-}"

is_trusted_actor() {
  case "$1" in
    cbusillo | shiny-code-bot) return 0 ;;
    *) return 1 ;;
  esac
}

deny() {
  echo "::error title=Runner host policy denied this job::$1"
  exit 1
}

[[ "$repository" == "$expected_repository" ]] || deny "Unexpected repository."
is_trusted_actor "$actor" || deny "The initiating actor is not trusted."
is_trusted_actor "$triggering_actor" || deny "The triggering actor is not trusted."

echo "Runner host policy authorized repository=$repository actor=$actor triggering_actor=$triggering_actor."
