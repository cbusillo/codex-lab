# Workflow Strategy

The workflows in this directory are split so that pull requests get fast, review-friendly signal while `main` still gets the full cross-platform verification pass.

## Pull Requests

- `blocking-ci.yml` is a bounded, public-fork-safe merge gate. Everything in
  its reusable-workflow graph runs on standard GitHub-hosted runners.
- `rust-ci.yml` keeps the Cargo-native PR checks intentionally small:
  - `cargo fmt --check`
  - `cargo shear`
  - `tools/argument-comment-lint` package tests when the lint or its workflow wiring changes
- `sdk.yml` runs the Python SDK suite plus TypeScript SDK build and lint checks.
  The TypeScript tests that spawn a real Codex binary run after merge instead
  of compiling the full Bazel/V8 graph on an ephemeral PR runner.
- `codex-lab-app.yml` builds the macOS ARM64 `Codex Lab.app` distribution when
  fork-owned packaging, workflow, or Rust CLI paths change. Pull-request builds
  use the `ci-app` Cargo profile and run only for branches in this repository;
  release builds retain the full release profile.
- Repository policy, spelling, dependency, and workflow-routing checks remain
  merge-blocking through their dedicated reusable workflows.

## Post-Merge On `main`

- `bazel.yml` compiles the full Bazel graph and runs Bazel clippy plus
  release-build verification on the trusted persistent Linux runner. Runtime
  tests stay in `rust-ci-full.yml`, where each platform has the dependencies
  and isolation expected by the test suite.
- `rust-ci-full.yml` is the full Cargo-native verification workflow.
  It keeps the heavier checks off the PR path while still validating them after merge:
  - the full Cargo `clippy` matrix
  - the full Cargo `nextest` matrix via per-platform archive-backed shards
  - Windows ARM64 nextest archives cross-compiled on Windows x64, then replayed on native Windows ARM64 shards
  - release-profile Cargo builds
  - cross-platform `argument-comment-lint`
  - Linux remote-env tests
- `sdk-integration.yml` builds Codex with Bazel and runs the TypeScript SDK
  integration tests against that real binary on the trusted Linux runner.
- `v8-canary.yml` keeps the upstream V8 artifact and source-build matrix visible
  after merge.

## Runner Ownership

- Merge-blocking workflows use standard GitHub-hosted runners so public fork
  pull requests never execute on persistent Codex Lab machines.
- Trusted postmerge, app, and release workflows may use the repository-scoped
  `[self-hosted, codex-lab-linux]`, `macos-codex-lab`, or
  `[self-hosted, macOS, ARM64, codex-lab-app]` labels. These fork-owned labels
  are intentionally explicit instead of imitating upstream organization runner
  groups or renaming a persistent runner to an upstream alias.
- Upstream Windows Bazel jobs require authenticated RBE and custom Windows exec
  toolchains, so they are not part of public-fork blocking CI. `rust-ci-full.yml`
  retains Windows validation after merge on GitHub-hosted Windows runners.
- `.github/scripts/verify_blocking_ci_runner_routing.py` follows the reusable
  workflow graph from `blocking-ci.yml` and rejects organization runner groups,
  persistent self-hosted runners, billable macOS large runners, and unsupported
  platform aliases.

## Inherited Upstream Release Publishing

- `r2-release.yml` mirrors `openai/codex` release assets into the upstream
  Cloudflare R2 `releases` bucket. It downloads from a hard-coded upstream
  repository and uploads with whatever R2 credentials the calling repository
  holds, so running it in this fork would republish upstream assets under
  Codex Lab credentials.
- Both the `publish-r2` caller in `rust-release.yml` and the reusable workflow
  itself are pinned to `github.repository == 'openai/codex'`, and
  `.github/scripts/publish_r2_release.py` fails closed on `GITHUB_REPOSITORY`
  before it reads any credential.
- `.github/scripts/verify_upstream_only_release_publishing.py` keeps those
  guards in place as upstream snapshots land.
- Codex Lab's own releases go through `codex-lab-release.yml`, which builds
  packed `.dSYM` sidecars and strips the managed engine before signing.

## Rule Of Thumb

- Keep the hosted PR graph cold-start bounded; a check that requires the full
  Bazel/V8 graph belongs in trusted postmerge CI.
- Keep `rust-ci.yml` and `sdk.yml` fast enough that they do not dominate PR latency.
- Preserve heavy Bazel, Cargo matrix, and real-binary SDK coverage in the
  postmerge workflows rather than deleting it.

## Developer Artifacts

High-churn self-hosted runner data can live under a host-managed artifact root
configured through `CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT`:

- `github-actions/runners/` contains self-hosted runner installations.
- `github-actions/cache/` contains reusable caches that survive checkout
  cleanup.
- `github-actions/tmp/` contains disposable workflow scratch data.

Workflow-specific caches add owner, repository, and workflow leaves under the
cache directory. `codex-lab-app.yml`, for example, uses
`github-actions/cache/<owner>/<repo>/codex-lab-app/` as its Cargo target cache
when the artifact root is configured and available.

## Distribution Contract

`codex-lab-app.yml` uploads one artifact containing:

- `codex-lab-app-aarch64-apple-darwin.zip`
- `codex-lab-shim-aarch64-apple-darwin.zip`
- `codex-lab-engine-aarch64-apple-darwin.zip`
- `SHA256SUMS`
- `codex-lab-distribution.json`

The manifest is the installer and updater contract. It identifies the app zip
as the canonical app update, the shim as its companion launcher, and the engine
as the managed supervisor execution unit. It also records extracted sibling,
`CODEX_LAB_APP_PATH`, and `/Applications` layouts. Pull-request artifacts remain
unsigned and are packaging-validation inputs, not publishable releases.

## Release Publication

`codex-lab-release.yml` is the Codex Lab-owned release authority. It builds the
macOS ARM64 app, shim, and engine, signs and verifies the engine on the trusted
macOS runner, validates the staged distribution on `ubuntu-latest`, and grants
`contents: write` only to the separate publication job.

The signed engine contract pins the executable digest, source commit, version,
stable identifier, TeamIdentifier, hardened runtime, and required V8
entitlements. Manual dispatch with `publish: false` performs a complete signed
dry run without creating a release. Publishing is restricted to explicit manual
dispatches from the default branch and creates a public GitHub prerelease; it
does not use upstream R2, package identities, release domains, or credentials.

Release tags use the isolated namespace:

```shell
codex-lab-vX.Y.Z
codex-lab-vX.Y.Z-lab.N
```
