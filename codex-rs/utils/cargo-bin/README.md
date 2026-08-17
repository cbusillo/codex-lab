# codex-utils-cargo-bin runfiles strategy

We disable the directory-based runfiles strategy and rely on the manifest
strategy across all platforms. This avoids Windows path length issues and keeps
behavior consistent in local and remote builds on all platforms. Bazel sets
`RUNFILES_MANIFEST_FILE`, and the `codex-utils-cargo-bin` helpers use the
`runfiles` crate to resolve runfiles via that manifest.

Function behavior:
- `cargo_bin`: reads `CARGO_BIN_EXE_*` environment variables (set by Cargo or
  Bazel) and resolves them via the runfiles manifest when `RUNFILES_MANIFEST_FILE`
  is present. Outside Bazel, it accepts absolute paths from `CARGO_BIN_EXE_*`,
  then checks the current Cargo target directory for an already-built binary.
  Missing binaries return `CargoBinError::NotFound` rather than panicking.
- `find_resource!`: used by tests to locate fixtures. It chooses the Bazel
  runfiles resolution path when `RUNFILES_MANIFEST_FILE` is set, otherwise it
  falls back to a `CARGO_MANIFEST_DIR`-relative path for Cargo runs.

Background:
- https://bazel.build/docs/runfiles
- https://bazel.build/docs/runfiles#runfiles-manifest
