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

`ObservedOwnerControlHost::current` exposes a sealed read-only description of
only the capabilities this library can observe. It always reports no principal
separation, unproven caller-supplied custody, no gesture source, no server
corroboration, and the derived `self_asserted` tier. Callers can provide opaque
host and principal identifiers, but cannot construct or deserialize an observed
descriptor with stronger claims. `OwnerControlEnrollmentIntent` binds that
observation to one validated channel binding and remains inert data: it performs
no key generation, signing, transport, discovery, environment lookup, or runtime
mutation and rejects published conformance keys.

Future process/IPC/UI/keyring/Launchplane transport adoption, including the
channel-facing work tracked by `#795`, remains explicitly deferred. Constructing
or verifying an envelope is not runtime authorization.
