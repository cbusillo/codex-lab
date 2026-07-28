# Workflow Strategy

The workflows in this directory are split so that pull requests and `main`
pushes get fast, review-friendly signal while comprehensive cross-platform
verification runs on a nightly or explicitly requested cadence.

## Pull Requests

- `blocking-ci.yml` is a bounded, public-fork-safe merge gate. Everything in
  its reusable-workflow graph runs on standard GitHub-hosted runners.
- `rust-ci.yml` keeps the Cargo-native PR checks intentionally small:
  - `cargo fmt --check`
  - `cargo shear`
  - `tools/argument-comment-lint` package tests when the lint or its workflow wiring changes
- `sdk.yml` runs the Python SDK suite plus TypeScript SDK build and lint checks.
  The TypeScript tests that spawn a real Codex binary run in the full suite
  instead of compiling the full Bazel/V8 graph on an ephemeral PR runner.
- `codex-lab-app.yml` builds the macOS ARM64 `Codex Lab.app` distribution when
  fork-owned packaging, workflow, or Rust CLI paths change. Pull-request builds
  reject any initiating or triggering actor outside the trusted allowlist on a
  hosted runner, require a same-repository branch, then check out the exact
  pull-request head SHA. The host-managed pre-job hook independently enforces
  the same actor boundary before repository steps run on the macOS lane.
  Release builds retain the full release profile.
- Repository policy, spelling, dependency, and workflow-routing checks remain
  merge-blocking through their dedicated reusable workflows.

## Full Verification

- `full-ci.yml` is the nightly and manual entrypoint for the heavy workflow
  fan-out. A newer full run cancels any older in-progress run for the same ref.
  Matrices and inner test/build tools stop after their first failure instead of
  spending the remaining runner budget on work that cannot make the suite green.
- `bazel.yml` compiles the full Bazel graph and runs Bazel clippy plus
  release-build verification on the trusted persistent Linux runner. Runtime
  tests stay in `rust-ci-full.yml`, where each platform has the dependencies
  and isolation expected by the test suite.
- `rust-ci-full.yml` is the full Cargo-native verification workflow.
  It keeps the heavier checks off the PR and per-merge paths while still
  validating them in the full suite:
  - the full Cargo `clippy` matrix
  - the full Cargo `nextest` matrix via per-platform archive-backed shards
  - Windows ARM64 nextest archives cross-compiled on Windows x64, then replayed on native Windows ARM64 shards
  - release-profile Cargo builds
  - cross-platform `argument-comment-lint`
  - Linux remote-env tests
- `sdk-integration.yml` builds Codex with Bazel and runs the TypeScript SDK
  integration tests against that real binary on the trusted Linux runner.
- `v8-canary.yml` keeps the upstream V8 artifact and source-build matrix visible
  in the full suite and on relevant pull requests.
- Bazel, Rust, SDK integration, and V8 remain independent top-level suites and
  start in parallel. GitHub Actions does not provide native cross-workflow
  fail-fast cancellation, so each suite fails fast internally without
  serializing the successful path.

## Release Gate

- `codex-lab-release.yml` runs the same Bazel, Rust, SDK integration, and V8
  reusable workflows on the exact selected release ref after validating release
  metadata and before building release artifacts.
- A recent nightly is useful evidence but never substitutes for this exact-ref
  release gate. Publishing remains downstream of both full verification and
  artifact validation.

## Runner Ownership

- Merge-blocking workflows use standard GitHub-hosted runners so public fork
  pull requests never execute on persistent Codex Lab machines.
- Trusted full-suite, app, and release workflows may use the repository-scoped
  `[self-hosted, codex-lab-linux]`, `macos-codex-lab`, or
  `[self-hosted, macOS, ARM64, codex-lab-app]` labels. These fork-owned labels
  are intentionally explicit instead of imitating upstream organization runner
  groups or renaming a persistent runner to an upstream alias.
- Every persistent-runner workflow first calls
  `authorize-self-hosted.yml`, which requires both `github.actor` and
  `github.triggering_actor` to be either `cbusillo` or `shiny-code-bot` before a
  self-hosted job can be assigned. Each persistent host also installs
  `.github/scripts/authorize-self-hosted-runner-job.sh` outside the runner work
  tree as an `ACTIONS_RUNNER_HOOK_JOB_STARTED` hook; `chris-testing` keeps that
  copy root-owned. Changing a workflow therefore cannot bypass the same
  repository and actor allowlist before repository code executes.
- `chris-testing` exposes four Codex Lab lanes named `chris-testing-codex`
  through `chris-testing-codex-4`. Each carries the shared
  `codex-lab-linux` label, runs under a lane-specific service account, and
  uses a separate runner install, work directory, home directory, Cargo target,
  Bazel output root, and temporary tree. The host budgets ten CPUs and 24 GiB
  per lane, leaving ten CPUs and 32 GiB available for the host and other runner
  fleets when all four Codex Lab lanes are active. Remote-environment tests
  require the host Docker daemon, so only these allowlisted lane accounts share
  the existing Docker group. External cache keys include the runner instance,
  and each lane uses its own sccache server endpoint, so restored archives and
  compiler daemons cannot cross lane homes.
- Upstream Windows Bazel jobs require authenticated RBE and custom Windows exec
  toolchains, so they are not part of public-fork blocking CI. `rust-ci-full.yml`
  retains Windows validation in the full suite on GitHub-hosted Windows runners.
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
- `publish-npm` (the `@openai` npm scope), `winget` (the `OpenAI.Codex` manifest
  via the `openai-oss-forks` winget-pkgs fork), and `deploy-dev-website` (the
  developers.openai.com Vercel deploy hook) are pinned to
  `github.repository == 'openai/codex'` for the same reason: they mutate
  OpenAI-owned external state, not this repository's.
- `.github/scripts/verify_upstream_only_release_publishing.py` keeps those
  guards in place as upstream snapshots land. It finds upstream-owned mutations
  by fingerprint (scope, manifest identifier, credential name) rather than by
  job name, so renaming or copying a job cannot drop its guard.
- `scripts/install/install.sh` and `install.ps1` resolve every download from
  `openai/codex`, so `rust-release.yml` stages them as release assets only in
  that repository. `.github/scripts/verify_release_installer_provenance.py`
  enforces that and the matching rule for Codex Lab: `codex-lab-release.yml`
  publishes only `codex-lab-*` assets plus `SHA256SUMS`.
- `.github/actions/setup-rusty-v8` downloads its `rusty-v8-v*` artifacts from
  `github.repository` by default, so a fork never links V8 blobs published by
  another repository. Pass `artifact-repository` to opt into a different source;
  `.github/scripts/download-rusty-v8-artifacts.sh` validates that input and
  `GITHUB_SERVER_URL` before either reaches a URL.
- `rust-ci-full.yml` and its `rust-ci-full-nextest-platform.yml` reusable
  workflow explicitly read the exact-version, checksummed artifacts from
  `openai/codex` because they only compile and test source; they publish nothing.
  `rust-release.yml` deliberately keeps the fail-closed default so a fork
  release cannot redistribute V8 blobs published by another repository.
- Codex Lab's own releases go through `codex-lab-release.yml`, which builds
  packed `.dSYM` sidecars and strips the managed engine before signing.

## Rule Of Thumb

- Keep the hosted PR graph cold-start bounded; a check that requires the full
  Bazel/V8 graph belongs in trusted full CI.
- Keep `rust-ci.yml` and `sdk.yml` fast enough that they do not dominate PR latency.
- Preserve heavy Bazel, Cargo matrix, and real-binary SDK coverage in the
  scheduled and manually dispatched full suite rather than deleting it.

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

## Signing Key Exposure: Open Operational Gate (#343)

This is a known, unresolved exposure. It is documented here instead of being
papered over with a code change that would not actually close it.

`codex-lab-release.yml` signs the managed engine with

```shell
security unlock-keychain -p "" "$HOME/Library/Keychains/login.keychain-db"
```

The Developer ID Application key therefore lives in the runner user's login
keychain behind an **empty password**. `codex-lab-app.yml` is pull-request
triggered and runs on the *same* `[self-hosted, macOS, ARM64, codex-lab-app]`
runner and the same user account. Every PR build executes repository-authored
code on that host -- `build.rs`, `scripts/build_codex_lab_app.py`,
`scripts/codex_lab_package/smoke.py`, Cargo build scripts of any dependency.
Any of them can run the same one-line unlock and sign arbitrary bytes with the
Codex Lab Developer ID.

`codex-lab-app.yml` is restricted to branches in this repository
(`github.event.pull_request.head.repo.full_name == github.repository`), so this
is not open to public forks. It is still a full compromise path for anyone who
can push a branch here, and it is not mitigated by anything in the workflows.

No code-only fix closes it. The exposure comes from *one host, one user account,
one unlocked keychain* shared between an untrusted-input build and a signing
operation. Closing it requires an operator action, not a workflow edit:

1. Move the Developer ID key out of the login keychain into a dedicated signing
   keychain with a real password supplied as a repository secret, **and**
2. Give the release signing job its own runner label so PR builds never execute
   on the host that holds the signing keychain.

Until both land, treat the Codex Lab signing identity as reachable by anyone
with push access. Track this on
[cbusillo/codex-lab#343](https://github.com/cbusillo/codex-lab/issues/343).

`.github/scripts/test_codex_lab_signing_exposure.py` keeps the gate honest: it
fails if signing spreads to a pull-request-triggered workflow, if the release
workflow gains a pull-request trigger, or if this section disappears while the
empty-password unlock is still in the tree.
