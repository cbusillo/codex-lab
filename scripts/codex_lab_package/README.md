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
same embedded binary used by Desktop mode.
