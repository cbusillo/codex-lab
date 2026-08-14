# Workflow Strategy

The workflows in this directory are split so that pull requests and `main`
pushes get fast, review-friendly signal while comprehensive verification runs
on a nightly or explicitly requested cadence.

## Temporary Apple Silicon-Only Mode

As of July 31, 2026, every active CI, canary, app-build, and release execution
path is limited to macOS ARM64. Linux, Windows, and Intel macOS workflows are
temporarily unreachable while Codex Lab prioritizes Apple Silicon development.

- `verify_apple_silicon_workflows.py` follows every active workflow entrypoint
  through its local reusable-workflow calls and rejects non-Apple runners,
  targets, containers, and release platforms.
- Windows-only reusable workflows remain `workflow_call`-only with no active
  callers so the old implementation is recoverable without consuming CI.
- The upstream multi-platform Rust and Python release entrypoints are also
  `workflow_call`-only. Codex Lab release publishing remains active through
  `codex-lab-release.yml`.
- Issue, contributor, translation, and CLA automation is outside this platform
  policy because it does not build, test, package, or release product code.

Restore another platform only by updating its workflow lanes, this document,
and the policy verifier together. Track the temporary mode and recovery decision
in [#517](https://github.com/cbusillo/codex-lab/issues/517).

## Pull Requests

- `blocking-ci.yml` is a bounded, public-fork-safe merge gate. Everything in
  its reusable-workflow graph runs on GitHub-hosted Apple Silicon runners.
- `rust-ci.yml` keeps the Cargo-native PR checks intentionally small:
  - `cargo fmt --check`
  - `cargo shear`
  - one hosted macOS ARM64 `cargo check --workspace --tests` compile gate
  - `tools/argument-comment-lint` package tests when the lint or its workflow wiring changes
- `codex-lab-bazel-analysis.yml` runs only for Bazel/Rust/release-relevant
  changes. It analyzes the release target set with `--nobuild` on hosted Apple
  Silicon, uses non-fatal repository-cache restore/save, and feeds the existing
  `blocking-ci / CI required` check instead of adding another required status.
  The initial August 14, 2026 measurement on 718 targets was 11.11 seconds with
  an empty repository cache and 0.91 seconds warm, below the decision thresholds
  recorded in issue #651. The workflow enforces the 25-minute analysis ceiling;
  the warm timing remains measurement evidence rather than a second CI run.
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
- `repo-checks.yml` statically verifies cross-package/root Bazel compile-data
  labels, producer exports, and SQLx migration globs, and runs a hermetic npm
  expansion/native-staging smoke test. These checks catch release metadata
  wiring errors without invoking Bazel or downloading release artifacts.

## Full Verification

- `full-ci.yml` is the nightly and manual entrypoint for the heavy workflow
  fan-out. A newer full run cancels any older in-progress run for the same ref.
  Matrices and inner test/build tools continue after individual failures so one
  run returns the complete actionable failure inventory.
- `bazel.yml` compiles the full Bazel graph and runs Bazel clippy plus
  release-build verification on the trusted persistent Apple Silicon runner. Runtime
  tests stay in `rust-ci-full.yml`, where each platform has the dependencies
  and isolation expected by the test suite.
- `rust-ci-full.yml` is the macOS ARM64 Cargo-native verification workflow used
  by both full CI and the current dogfood release gate. It keeps the heavier
  checks off the PR and per-merge paths while validating the active target:
  - Cargo `clippy` in development and release profiles
  - the Cargo `nextest` suite via archive-backed shards
  - release-profile Cargo builds
  - Apple Silicon `argument-comment-lint`
- `sdk-integration.yml` builds Codex with Bazel and runs the TypeScript SDK
  integration tests against that real binary on the trusted Apple Silicon runner.
- `v8-canary.yml` keeps the Apple Silicon upstream V8 artifact pair visible in
  the release gate, full suite, and relevant pull requests.
- Bazel, Rust, SDK integration, and V8 remain independent top-level suites and
  start in parallel. Full verification optimizes for diagnostic completeness;
  the bounded pull-request gate is responsible for fast rejection.

## Release Gate

- `codex-lab-release.yml` runs Bazel, SDK integration, and the supported
  macOS ARM64 Rust and V8 workflows on the exact selected release ref after
  validating release metadata and before building release artifacts.
- A recent nightly is useful evidence but never substitutes for this exact-ref
  release gate. Publishing remains downstream of both full verification and
  artifact validation.

## Runner Ownership

- Merge-blocking workflows use GitHub-hosted Apple Silicon runners so public
  fork pull requests never execute on persistent Codex Lab machines.
- Trusted full-suite, app, and release workflows may use the repository-scoped
  `macos-codex-lab` or
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
- Persistent lane storage is limited to reusable Cargo, Bazel, repository, and
  sccache data. Nextest extraction trees and other `TEMP`/`TMP` data use
  runner-managed temporary storage on Unix and the fresh per-job Dev Drive on
  Windows, so jobs cannot accumulate per-run archives in lane caches.
- Upstream Windows Bazel jobs require authenticated RBE and custom Windows exec
  toolchains, so they are not part of public-fork blocking CI.
  `rust-ci-full-windows.yml` retains Windows validation in the full suite on
  GitHub-hosted Windows runners.
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
- `rust-ci-full-argument-comment-lint.yml`, `rust-ci-full-lint-build.yml`, and
  `rust-ci-full-nextest-platform.yml` explicitly read the exact-version,
  checksummed artifacts from `openai/codex` because they only compile and test
  source; they publish nothing.
  `rust-release.yml` deliberately keeps the fail-closed default so a fork
  release cannot redistribute V8 blobs published by another repository.
- Codex Lab's own releases go through `codex-lab-release.yml`, which builds
  packed `.dSYM` sidecars and strips the managed engine before signing.

## Rule Of Thumb

- Keep the hosted PR graph cold-start bounded; a check that requires the full
  Bazel/V8 graph belongs in trusted full CI.
- Keep `rust-ci.yml` and `sdk.yml` fast enough that they do not dominate PR latency.
- Preserve heavy Bazel, Cargo, and real-binary SDK coverage in the scheduled
  and manually dispatched Apple Silicon suite.

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

The trusted Apple Silicon V8 canary and release jobs use separate persistent
Bazel repository and disk caches through
`scripts/github/configure-codex-lab-bazel-cache.sh`. Each disk cache is capped
at 80 GB so both caches fit within the artifact volume quota with room for
repository caches and normal artifact growth.

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
macOS runner, validates the staged distribution on `macos-26`, and grants
`contents: write` only to the separate publication job.

The signed engine contract pins both binary digests, stable identifiers,
TeamIdentifier, hardened runtime, and exact V8 entitlements; it additionally
pins the CLI's source commit and version through structured provenance. Manual
dispatch with `publish: false` performs a complete signed dry run without
creating a release. Publishing is restricted to explicit manual dispatches from
the default branch and creates a public GitHub prerelease; it does not use
upstream R2, package identities, release domains, or credentials.

Release tags use the isolated namespace:

```shell
codex-lab-vX.Y.Z
codex-lab-vX.Y.Z-lab.N
```

## Signing Key Exposure: Open Operational Gate (#614)

This is a known, unresolved exposure. It is documented here instead of being
papered over with a code change that would not actually close it.

`codex-lab-release.yml` requires the trusted runner login keychain to be
already unlocked before it signs the managed engine:

```shell
security show-keychain-info "$HOME/Library/Keychains/login.keychain-db"
```

The Developer ID Application key therefore lives in the runner user's login
keychain, whose password is intentionally not embedded in the workflow.
`codex-lab-app.yml` is pull-request triggered and runs on the _same_
`[self-hosted, macOS, ARM64, codex-lab-app]` runner and the same user account.
Every PR build executes repository-authored code on that host -- `build.rs`,
`scripts/build_codex_lab_app.py`,
`scripts/codex_lab_package/smoke.py`, Cargo build scripts of any dependency.
While the interactive runner session keeps the login keychain unlocked, any of
them can sign arbitrary bytes with the Codex Lab Developer ID.

`codex-lab-app.yml` is restricted to branches in this repository
(`github.event.pull_request.head.repo.full_name == github.repository`), so this
is not open to public forks. It is still a full compromise path for anyone who
can push a branch here, and it is not mitigated by anything in the workflows.

No code-only fix closes it. The exposure comes from _one host, one user account,
one unlocked keychain_ shared between an untrusted-input build and a signing
operation. Closing it requires operator actions, not another workflow-only edit:

1. Move the Developer ID key out of the login keychain into a dedicated signing
   keychain with credentials supplied only to the release job, **and**
2. Give the release signing job its own runner label so PR builds never execute
   on the host that holds the signing keychain.

Until both land, treat the Codex Lab signing identity as reachable by anyone
with push access. Track this on
[cbusillo/codex-lab#614](https://github.com/cbusillo/codex-lab/issues/614).

`.github/scripts/test_codex_lab_signing_exposure.py` keeps the gate honest: it
fails if signing spreads to a pull-request-triggered workflow, if the release
workflow gains a pull-request trigger, if a keychain-password assumption enters
the workflow, or if this section disappears while the shared-runner exposure
remains.
