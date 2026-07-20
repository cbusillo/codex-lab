# Codex Lab Desktop Launcher Packaging

This helper builds a macOS `Codex Lab.app` launcher bundle. The bundle does not
contain or modify OpenAI's signed desktop app. Instead, it embeds a Codex Lab
CLI binary, binds its source commit, version, and SHA-256 digest, sets
`CODEX_CLI_PATH` to that exact path, and launches through LaunchServices.

The launcher accepts only intact `com.openai.codex` bundles signed by OpenAI
team `2DC432GLL2`. After an optional build-time override, it checks system and
user `ChatGPT.app` installs, then legacy `Codex.app` installs. It never patches,
re-signs, or redistributes the official bundle. If that app is already running,
the launcher fails closed; quit it before launching `Codex Lab.app` so the new
process inherits `CODEX_CLI_PATH`.

Example:

```shell
scripts/build_codex_lab_app.py \
  --codex-bin codex-rs/target/release/codex-lab \
  --app-dir /tmp/Codex\ Lab.app \
  --shim-dir /tmp/codex-lab-bin \
  --force
```

The builder reads version, source commit, and clean/dirty state directly from
the embedded CLI. It rejects dirty builds or explicitly supplied metadata that
does not match the binary, so launcher failures are caught while packaging.
The generated bundle includes a distinct Codex Lab icon.

The optional shim directory receives a `codex-lab` wrapper that executes the
same embedded binary used by Desktop mode. The shim searches for `Codex Lab.app`
in these locations, in order:

1. `CODEX_LAB_APP_PATH`, for explicit overrides.
2. `../Codex Lab.app` relative to the shim, for extracted artifact layouts.
3. The app path embedded when the shim was generated.
4. `/Applications/Codex Lab.app`.
5. `~/Applications/Codex Lab.app`.

## Live desktop provenance smoke

After building or installing `Codex Lab.app`, run the live smoke check on macOS:

```shell
python3 scripts/codex_lab_package/live_smoke.py \
  "/Applications/Codex Lab.app"
```

The check launches a fresh GUI instance and emits bounded JSON only after a new
descendant `app-server` executable resolves exactly to the embedded CLI. The
evidence includes its PID, selected app, and fixed source/build provenance.

The GitHub workflow uploads `codex-lab-distribution.json` beside the app zip,
shim zip, and `SHA256SUMS`. The manifest records artifact roles, sizes,
checksums, source workflow metadata, supported install layouts, release tags,
download URLs when published, and the current signing state. Codex Lab artifacts
are currently marked `signed: false` and `notarized: false` until a later
signing/notarization stage is implemented.

Packaging workflows bind the static smoke to the expected source commit before
the interactive GUI smoke is performed.

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

To check for a newer published Lab release without changing the install, run:

```shell
scripts/install_codex_lab.py --check
```

To update an existing install in place, run:

```shell
scripts/install_codex_lab.py --update
```

`--update` reads the recorded install state, preserves the installed app path and
shim path, and replaces only when a newer published Lab release is available.

The installer downloads the manifest, `SHA256SUMS`, app zip, and shim zip into a
temporary staging directory. It validates the manifest shape, requires artifact
URLs to be siblings of the manifest URL, checks artifact sizes and SHA-256
hashes, rejects unsafe zip members, smoke-checks the staged app and shim, then
replaces the requested install paths. Existing targets are refused unless
`--force` is supplied.

Codex Lab release artifacts are currently unsigned and unnotarized. This
installer is a manual Lab installer/update path; silent automatic updates should
wait for signed or notarized artifacts, or a signed manifest.
