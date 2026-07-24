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
- Repository policy, spelling, dependency, and workflow-routing checks remain
  merge-blocking through their dedicated reusable workflows.

## Post-Merge On `main`

- `bazel.yml` runs the full Bazel test, clippy, and release-build verification
  on the trusted persistent Linux runner plus hosted macOS runners. Keeping the
  full macOS Bazel graph hosted protects the app/release runner's local disk.
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
  `[self-hosted, codex-lab-linux]` and `[self-hosted, macos-codex-lab]` labels.
  These fork-owned labels are intentionally explicit instead of imitating
  upstream organization runner groups.
- Upstream Windows Bazel jobs require authenticated RBE and custom Windows exec
  toolchains, so they are not part of public-fork blocking CI. `rust-ci-full.yml`
  retains Windows validation after merge on trusted repository-owned runners.
- `.github/scripts/verify_blocking_ci_runner_routing.py` follows the reusable
  workflow graph from `blocking-ci.yml` and rejects organization runner groups,
  persistent self-hosted runners, billable macOS large runners, and unsupported
  platform aliases.

## Rule Of Thumb

- Keep the hosted PR graph cold-start bounded; a check that requires the full
  Bazel/V8 graph belongs in trusted postmerge CI.
- Keep `rust-ci.yml` and `sdk.yml` fast enough that they do not dominate PR latency.
- Preserve heavy Bazel, Cargo matrix, and real-binary SDK coverage in the
  postmerge workflows rather than deleting it.
