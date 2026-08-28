# Owner Control Host

This library-only crate hosts an inert, fail-closed owner-confirmation flow over
the strict `codex-owner-control-contract` types. It validates canonical
challenge and channel-binding inputs, exposes the server-authored review, and
requires a consuming one-shot gesture before requesting a signature from an
injected custody implementation.

It is intentionally unrouted and non-authorizing. It has no process, IPC, UI,
keyring, Launchplane transport, network, socket, configuration, tracing, CLI,
TUI, core, tools, MCP, browser, Code Bridge, or app-server integration. A host
must supply time, replay storage, and signature custody; this crate never loads
keys or performs I/O.

Future process/IPC/UI/keyring/Launchplane transport adoption, including the
channel-facing work tracked by `#795`, remains explicitly deferred. Constructing
or verifying an envelope is not runtime authorization.
