# Codex Lab Workflow Strategy

This fork keeps upstream workflows available, but the automatic PR signal is
owned by Codex Lab. Upstream's full CI and release workflows assume OpenAI
runner groups, secrets, and release infrastructure that this fork does not own.

## Pull Requests

- `ci.yml` runs cheap repository sanity checks plus Codex Lab package-builder
  unit and smoke tests.
- `codex-lab-app.yml` builds the macOS ARM64 `Codex Lab.app` artifact on the
  self-hosted macOS runner when packaging files or the workflow change.
- `blob-size-policy.yml`, `codespell.yml`, and `cargo-deny.yml` are retained as
  lightweight inherited checks while they remain fork-safe.

## Manual Upstream Parity Checks

The inherited heavyweight workflows are `workflow_dispatch` only in this fork:

- `bazel.yml`
- `rust-ci.yml`
- `sdk.yml`
- `v8-canary.yml`

Run these manually when a change needs upstream-style validation or touches the
areas those workflows own. Keep them out of the default PR path until this fork
has matching runner capacity, secrets, and branch-protection expectations.

## Local Runner Contract

`codex-lab-app.yml` expects a self-hosted macOS ARM64 runner with these labels:

- `self-hosted`
- `macOS`
- `ARM64`
- `codex-lab-app`

The current local runner is `chris-mac-codex-release-1`. It must have Rust,
Python 3, Xcode command line tools, and macOS `ditto` available. The generated
Codex Lab app artifact is currently unsigned.
