# Code Bridge Service

Minimal local service skeleton for the Code Bridge protocol.

The service binds to loopback, writes a descriptor under the resolved Codex Lab
home, and requires the descriptor secret as a Bearer token before JSON payloads
are accepted. This crate intentionally stops at service mechanics: it does not
connect browser clients, app-server flows, Launchplane work requests, or product
adapters.

SSE clients may reconnect with `Last-Event-ID`; the service replays retained
matching event deliveries and targeted screenshot/control request-response
deliveries from a bounded in-memory buffer.
