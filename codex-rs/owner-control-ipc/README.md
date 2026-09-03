# Owner Control IPC

This library-only crate defines an inert, versioned local IPC seam for
presenting one validated owner-control challenge review. The wire protocol is a
single length-prefixed request and response. It accepts only the exact approval
request and channel-binding records, reconstructs the review from those
validated records, and returns bounded digests with that review.
This layer validates contract structure and cross-record binding only; it does
not authenticate that caller-delivered records originated from Launchplane.

The crate cannot produce an owner-control confirmation. The confirmation
request is deliberately wired to `DenyAllGestureSource`; the protocol has no
gesture, custody, signature, envelope, token, command, or dynamic-path field,
and every confirmation attempt returns `gesture_unavailable`. No response
variant carries authorization material.

On supported Unix systems the endpoint requires an injected absolute socket
path, an existing current-user-owned `0700` parent directory, a newly created
current-user-owned `0600` socket, and a same-UID peer. Existing socket paths are
never removed automatically, and the server rechecks directory and socket
identity before and after accepting a peer. Accepted streams use one absolute
read/write deadline for the complete request and response.
Windows and Unix targets without a supported peer-credential API return
`unsupported_platform`.

This crate has no binary, daemon, runtime integration, endpoint discovery,
configuration, environment lookup, Launchplane transport, keyring, logging,
tracing, network client, async runtime, app-server, core, tool, shell, MCP,
browser, Code Bridge, CLI, TUI, or authorization integration. Same-UID peer
checks do not isolate an owner process from an agent running as the same OS
user, authenticate the pathname to clients, or prevent same-UID replacement and
denial-of-service races. Neither peer credentials nor socket-path checks are
evidence of principal separation or client-path authenticity; the machine-checked
host provenance therefore remains `self_asserted` with no gesture source and no
custody proof. Genuine principal separation, owner UI, process
custody, and active route adoption remain follow-on work tracked by `#794` and
`#795`.
