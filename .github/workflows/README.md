# Codex Lab Workflow Strategy

This fork keeps upstream workflows available, but the automatic PR signal is
owned by Codex Lab. Upstream's full CI and release workflows assume OpenAI
runner groups, secrets, and release infrastructure that this fork does not own.

## Pull Requests

- `ci.yml` runs cheap repository sanity checks plus Codex Lab package-builder
  unit and smoke tests.
- `ci.yml` also runs an always-present extended-checks decision job. This job
  does not run expensive checks itself; it reports whether `codex-lab-app` and
  `exec-harness` are required for the changed paths and explains the matched
  files in the job summary.
- `codex-lab-app.yml` builds the macOS ARM64 `Codex Lab.app` artifact on the
  self-hosted macOS runner when packaging files, Rust CLI code, or the workflow
  change. PR builds use the faster `ci-app` Cargo profile; the release workflow
  retains the full release profile. The self-hosted job is guarded so it runs
  automatically only for branches in this repository or manual dispatches.
- `exec-harness.yml` runs Codex exec-harness scenarios on the self-hosted Linux
  runner when harness files, local harness helpers, Rust code, or the workflow
  change. The self-hosted job is guarded so it runs automatically only for
  branches in this repository or manual dispatches.
- `codespell.yml` and `cargo-deny.yml` are retained as lightweight inherited
  checks while they remain fork-safe.

The extended-checks routing map lives in `.github/extended-checks.json` and is
evaluated by `scripts/github/decide_extended_checks.py`. Keep this map
conservative: broad `codex-rs/**` routing is intentional until measured evidence
shows it is safe to narrow. When a workflow starts calling a new script or a
checked area moves, update the routing map in the same change. The fast CI
decision job validates that workflow-invoked scripts remain covered and that the
checked-in routing map matches the long workflows' `pull_request.paths` filters,
so stale routing fails visibly instead of silently skipping extended validation.

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

The runner must have Rust, Python 3, Xcode command line tools, and macOS
`ditto` available. The generated Codex Lab app artifact is currently unsigned.

`exec-harness.yml` expects a self-hosted Linux x64 runner with these labels:

- `self-hosted`
- `Linux`
- `X64`
- `codex-lab-linux`

### Developer Artifacts Volume

High-churn runner data can live under a host-managed artifact root when one is
configured. Keep the layout stable and purpose-based so future builds, local
automation, and cleanup scripts can share the volume without guessing what owns
each path. Configure the root through `CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT` in
the local shell or GitHub repository/environment variables.

- `$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT/github-actions/runners/` for
  self-hosted runner installations.
- `$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT/github-actions/cache/` for reusable
  caches that should survive checkout cleanup.
- `$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT/github-actions/tmp/` for disposable
  workflow scratch data that can be removed without losing build acceleration.

Workflow-specific caches should add owner/repo and workflow leaves under
`github-actions/cache/`. For example, `codex-lab-app.yml` uses
`$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT/github-actions/cache/<owner>/<repo>/codex-lab-app/`
as its Cargo target cache root on self-hosted runners, falling back to Cargo's
default target directory when no artifact root is configured or available.

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
