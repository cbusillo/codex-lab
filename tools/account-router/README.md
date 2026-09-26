# Stock Codex account router

Package scaffold for a sidecar that separates a shared Codex server's phone/control
login from model execution accounts. The owning work item is
[codex-lab #979](https://github.com/cbusillo/codex-lab/issues/979).

This package targets macOS and Linux hosts with a local Unix control socket.
Installation or import does not change credentials, configuration, or services.

From this directory:

```sh
uv sync --locked --group dev
uv run ruff check .
uv run ruff format --check .
```
