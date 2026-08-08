# Codex Lab Desktop Launcher Packaging

This helper builds a macOS `Codex Lab.app` launcher bundle. The bundle does not
contain or modify OpenAI's signed desktop app. Instead, it embeds a Codex Lab
CLI binary and binds its source commit, version, and SHA-256 digest. The official
app is bound to the persistent engine by setting `CODEX_HOME` and
`CODEX_LAB_HOME` to the same Lab home, setting
`CODEX_APP_SERVER_WS_URL=ws://127.0.0.1:4766/rpc`, and explicitly setting
`CODEX_CLI_PATH`, `CODEX_APP_SERVER_FORCE_CLI`, and
`CODEX_APP_SERVER_USE_LOCAL_DAEMON` to empty values. Current official clients
use a non-empty CLI path or `CODEX_APP_SERVER_FORCE_CLI=1` to select stdio.

The launcher accepts only intact `com.openai.codex` bundles signed by OpenAI
team `2DC432GLL2`. After an optional build-time override, it checks system and
user `ChatGPT.app` installs, then legacy `Codex.app` installs. It never patches,
re-signs, or redistributes the official bundle. If that app is already running,
the launcher fails closed; quit it before launching `Codex Lab.app` so the new
process inherits the websocket environment. The launcher also fails closed
unless launchd service `dev.everycode.codex-lab.app-server.v1` is running the
validated supervisor runner, the exact managed engine command, and the pinned
loopback listener. This prevents silent fallback to a bundled stdio app-server.
The embedded and managed engines must report the same bounded source/build
provenance. Inspect the installed service with:

```shell
launchctl print "gui/$(id -u)/dev.everycode.codex-lab.app-server.v1"
```

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
`--short-version` controls the GUI bundle `CFBundleShortVersionString` and may
differ from the embedded CLI version. Use `--embedded-cli-version` when release
automation wants to pin the expected backend version explicitly. The generated
bundle includes a distinct Codex Lab icon.

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

The check launches a fresh GUI instance and emits bounded JSON only after the
GUI is running beside the launchd-supervised websocket app-server. The embedded
and managed CLI builds must have matching fixed source/build provenance.

The GitHub workflow uploads `codex-lab-distribution.json` beside the app zip,
shim zip, managed-engine zip, and `SHA256SUMS`. The manifest records artifact
roles, sizes, checksums, source workflow metadata, supported install layouts,
release tags, download URLs when published, and the managed CLI and Code Mode
host digests, Developer ID identifiers, TeamIdentifiers, version, source commit,
and required V8 entitlements. PR app artifacts carry unsigned engine binaries
for package validation; published release manifests require both binaries to be
individually Developer ID signed.

Packaging workflows bind the static smoke to the expected source commit before
the interactive GUI smoke is performed.

## Installing a published release

The published-release installer provisions the app, optional shim, individually
signed managed CLI and Code Mode host, and the
`dev.everycode.codex-lab.app-server.v1` user LaunchAgent as one rollback-aware
transaction. No manual canary provisioning is required for a supported release.

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
shim path, installs the matching engine, and restarts the pinned supervisor only
when a newer published Lab release is available. It does not enable the upstream
standalone updater.

To remove the recorded install and restore any managed engine that predated the
first supported installer run, use:

```shell
scripts/install_codex_lab.py --uninstall
```

The installer downloads the manifest, `SHA256SUMS`, app zip, shim zip, and engine
zip into a temporary staging directory. It validates release URLs, sizes, and
SHA-256 hashes; rejects unsafe zip members; smoke-checks the app and shim; and
uses macOS code-signing inspection plus engine provenance to require the exact
binary digest, source commit, version, stable identifier, TeamIdentifier, and V8
JIT entitlement from the release metadata. It then replaces the engine, app,
shim, and state as a rollback set before installing and health-checking the
LaunchAgent. A provisioning failure restores the prior files and the
supervisor's own rollback restores its prior runner, plist, and load state.
Existing targets are refused unless `--force` is supplied.

The app and shim remain unsigned Lab launch surfaces. The managed engine is the
individually Developer ID signed execution boundary pinned by the supervisor.
