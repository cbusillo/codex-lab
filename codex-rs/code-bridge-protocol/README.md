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
- Retained SSE deliveries: 500, additionally bounded by service memory budget.
- Event text field: 4 KiB.
- Screenshot payload: 2 MiB after encoding.
- Screenshot dimensions: 4096 x 4096 maximum.
- Control timeout: 10 seconds maximum.
- Model-visible summary: 8 KiB maximum.
- Client label: 128 bytes.
- Provenance URLs: 512 bytes each.
- Provenance request and trace ids: 128 bytes each.
- Provenance environment label: 128 bytes.

Raw screenshots and high-volume logs are never injected directly into model
context. Consumers must summarize or explicitly request bounded artifacts.

## Product Boundaries

Core protocol metadata is generic: client ids, labels, source kind, repository
URL, issue/PR URL, request id, trace id, and environment label. Product-specific
Launchplane, discord-blue, or app-server fields do not belong in this crate.

Provenance is optional and must be safe to copy between local tools. Repository
and issue/PR provenance only accepts HTTPS identity links without
username/password, ports, query strings, fragments, localhost, or private IP
hosts so clients do not smuggle tokens, prompts, local file paths, topology
details, or raw work records into the bridge. Request id, trace id, and
environment label values are short ASCII tokens; environment labels should be
coarse labels such as `local-dev`, not hostnames, tenant names, or live topology.

Launchplane remains the authority for work requests and planning state. Code
Bridge only carries optional bounded provenance metadata that can correlate live
app events with outside work systems.
