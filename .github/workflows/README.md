# Codex Lab Workflow Strategy

This fork keeps upstream workflows available, but the automatic PR signal is
owned by Codex Lab. Upstream's full CI and release workflows assume OpenAI
runner groups, secrets, and release infrastructure that this fork does not own.

## Pull Requests

- `ci.yml` runs cheap repository sanity checks plus Codex Lab package-builder
  unit and smoke tests.
- `codex-lab-app.yml` builds the macOS ARM64 `Codex Lab.app` artifact on the
  self-hosted macOS runner when packaging files, Rust CLI code, or the workflow
  change. The self-hosted job is guarded so it runs automatically only for
  branches in this repository or manual dispatches.
- `codespell.yml` and `cargo-deny.yml` are retained as lightweight inherited
  checks while they remain fork-safe.

## Manual Upstream Parity Checks

The inherited heavyweight workflows are `workflow_dispatch` only in this fork:

- `bazel.yml`
- `rust-ci-full.yml`
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

### Developer Artifacts Volume

High-churn runner data should live under `/Volumes/Developer-Artifacts` when
that volume is mounted. Keep the layout stable and purpose-based so future
builds, local automation, and cleanup scripts can share the volume without
guessing what owns each path:

- `/Volumes/Developer-Artifacts/github-actions/runners/` for self-hosted runner
  installations.
- `/Volumes/Developer-Artifacts/github-actions/cache/` for reusable caches that
  should survive checkout cleanup.
- `/Volumes/Developer-Artifacts/github-actions/tmp/` for disposable workflow
  scratch data that can be removed without losing build acceleration.

Workflow-specific caches should add owner/repo and workflow leaves under
`github-actions/cache/`. For example, `codex-lab-app.yml` uses
`/Volumes/Developer-Artifacts/github-actions/cache/<owner>/<repo>/codex-lab-app/`
as its Cargo target cache root on self-hosted runners, falling back to Cargo's
default target directory when the artifact volume is unavailable.

## Codex Lab Distribution Contract

`codex-lab-app.yml` uploads these files in one artifact:

- `codex-lab-app-aarch64-apple-darwin.zip`
- `codex-lab-shim-aarch64-apple-darwin.zip`
- `SHA256SUMS`
- `codex-lab-distribution.json`

The distribution manifest is the contract for future installers and updaters.
It marks the app zip as the canonical app update unit, the shim zip as a
companion wrapper, and records supported layouts for extracted sibling installs,
`CODEX_LAB_APP_PATH` overrides, and `/Applications` installs. Artifacts remain
`signed: false` and `notarized: false` until the signing pipeline exists.

## Codex Lab Release Publication

`codex-lab-release.yml` builds the same macOS ARM64 distribution files and
stages them for GitHub Releases. It separates trust boundaries deliberately:

- the self-hosted macOS runner builds and uploads a workflow artifact with
  `contents: read` permissions;
- an `ubuntu-latest` validation job downloads the staged artifact, verifies
  checksums, and checks that the manifest has release metadata and download
  URLs. This validates internal consistency, not artifact provenance;
- a separate `ubuntu-latest` publish job has `contents: write` and creates a
  public prerelease only for explicit manual dispatches with `publish: true`.

Manual dispatch with `publish: false` is the dry-run path: it builds and
validates the release artifact set, including checking that the release tag is
available, without creating a GitHub Release. Publishing is restricted to manual
dispatches from the repository default branch. Published Codex Lab releases are
public prereleases and are not marked as latest while the artifacts remain
unsigned and unnotarized. Public prereleases are used so manifest `downloadUrl`
entries are immediately usable by installers and updaters.

Release IDs use this namespace:

```shell
codex-lab-vX.Y.Z
codex-lab-vX.Y.Z-lab.N
```
