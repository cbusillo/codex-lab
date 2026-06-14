# Code Bridge Protocol

This crate defines the first Code Bridge wire contract. It is intentionally
standalone: app-server, Launchplane, discord-blue, browser clients, and future
service code should integrate through this contract instead of making app-server
the high-volume telemetry host.

## Trust Model

- Protocol version: `code_bridge.v1`.
- The first service implementation must bind loopback-only, or use a local
  transport with equivalent trust properties.
- Startup writes or exposes a local descriptor containing the protocol version,
  endpoint, and auth secret. For Codex Lab, the descriptor lives under the
  resolved Codex Lab home at `code-bridge/descriptor.json`, not inside repo
  `.code/` or repo `.codex/`. Descriptor files must be readable only by the
  owner when the platform supports ownership permissions.
- Every client hello carries an auth secret before event/control payloads are
  accepted. Missing or invalid auth fails closed.
- Producers, subscribers, and service controllers are separate roles. A client
  only gets the capabilities granted in its hello response.

## First Message Families

The first slice includes only these message families:

- `hello`
- `heartbeat`
- `event`
- `subscribe`
- `ack`
- `error`
- `screenshotRequest` / `screenshotResponse`
- `controlRequest` / `controlResponse`

The first event families are limited to:

- `console`
- `error`
- `pageview`
- `screenshot`
- `controlResult`

The first control commands are limited to screenshot capture and bounded
JavaScript execution. JavaScript execution is capability-gated and intended for
local development clients only.

## Payload Caps

- Message payload: 64 KiB.
- Retained events: 500.
- Event text field: 4 KiB.
- Screenshot payload: 2 MiB after encoding.
- Screenshot dimensions: 4096 x 4096 maximum.
- Control timeout: 10 seconds maximum.
- Model-visible summary: 8 KiB maximum.

Raw screenshots and high-volume logs are never injected directly into model
context. Consumers must summarize or explicitly request bounded artifacts.

## Product Boundaries

Core protocol metadata is generic: client ids, labels, source kind, repository
URL, issue/PR URL, request id, trace id, and environment label. Product-specific
Launchplane, discord-blue, or app-server fields do not belong in this crate.

Launchplane remains the authority for work requests and planning state. Code
Bridge only carries optional bounded provenance metadata that can correlate live
app events with outside work systems.
