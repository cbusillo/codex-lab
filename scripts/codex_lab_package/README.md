# Codex Lab Desktop Launcher Packaging

This helper builds a macOS `Codex Lab.app` launcher bundle. The bundle does not
contain or modify OpenAI's Codex Desktop app. Instead, it embeds a Codex Lab CLI
binary, sets `CODEX_CLI_PATH` to that binary, and launches the official
`/Applications/Codex.app` through LaunchServices.

Example:

```shell
scripts/build_codex_lab_app.py \
  --codex-bin codex-rs/target/release/codex \
  --app-dir /tmp/Codex\ Lab.app \
  --shim-dir /tmp/codex-lab-bin \
  --force
```

The optional shim directory receives a `codex-lab` wrapper that executes the
same embedded binary used by Desktop mode. The shim searches for `Codex Lab.app`
in these locations, in order:

1. `CODEX_LAB_APP_PATH`, for explicit overrides.
2. `../Codex Lab.app` relative to the shim, for extracted artifact layouts.
3. The app path embedded when the shim was generated.
4. `/Applications/Codex Lab.app`.
5. `~/Applications/Codex Lab.app`.

The GitHub workflow uploads `codex-lab-distribution.json` beside the app zip,
shim zip, and `SHA256SUMS`. The manifest records artifact roles, sizes,
checksums, source workflow metadata, supported install layouts, release tags,
download URLs when published, and the current signing state. Codex Lab artifacts
are currently marked `signed: false` and `notarized: false` until a later
signing/notarization stage is implemented.

## Installing a published release

Use `scripts/install_codex_lab.py` to install or manually update Codex Lab from a
published release manifest:

```shell
scripts/install_codex_lab.py \
  --latest \
  --force
```

Use `--release-tag codex-lab-v0.0.0-lab.2` instead of `--latest` to pin a
specific release.

By default this installs `Codex Lab.app` into `~/Applications`, installs the
`codex-lab` shim into `~/.local/bin`, and writes installer state to
`~/Library/Application Support/Codex Lab/install-state.json`. Use `--app-dir`,
`--shim-dir`, and `--state-path` to choose different user-writable locations, or
`--no-shim` to skip shim installation.

To see which Codex Lab release is installed, read the recorded install state:

```shell
scripts/install_codex_lab.py --status
```

The installer downloads the manifest, `SHA256SUMS`, app zip, and shim zip into a
temporary staging directory. It validates the manifest shape, requires artifact
URLs to be siblings of the manifest URL, checks artifact sizes and SHA-256
hashes, rejects unsafe zip members, smoke-checks the staged app and shim, then
replaces the requested install paths. Existing targets are refused unless
`--force` is supplied.

Codex Lab release artifacts are currently unsigned and unnotarized. This
installer is a manual Lab installer/update path; silent automatic updates should
wait for signed or notarized artifacts, or a signed manifest.
