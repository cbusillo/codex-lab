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
3. `/Applications/Codex Lab.app`.
4. `~/Applications/Codex Lab.app`.

The GitHub workflow uploads `codex-lab-distribution.json` beside the app zip,
shim zip, and `SHA256SUMS`. The manifest records artifact roles, sizes,
checksums, source workflow metadata, supported install layouts, and the current
signing state. Codex Lab artifacts are currently marked `signed: false` and
`notarized: false` until a later signing/notarization stage is implemented.
