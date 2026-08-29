# Owner Control Contract

This standalone crate provides the first behavior-neutral Rust representation
of Launchplane's owner-control contract. It is not connected to CLI, core,
TUI, app-server, tools, MCP, Code Bridge, networking, helper processes, key
custody, IPC, runtime authorization, or other runtime behavior.

The vendored artifact at `contracts/owner-control-contract.json` is byte-for-byte
from `cbusillo/launchplane` commit
`9068747f4bedf56ebea82e361f6993f565e2599f`, merged into `main` by PR `#2273`
as merge commit `036725dcdd8786d10fd5f1a07ac79e89cb156166`. Its SHA-256 is
`e3e40e511f3246380291edd7bf3872847039c49c94121885e12fa6116a0b1fae`.

The public API exposes typed artifact loading, strict model validation, the
artifact's canonical JSON serializer, SHA-256 helpers, and Ed25519 signature
proof verification over published conformance envelopes. The v3 container also
strictly parses Launchplane's synthetic shadow-verification and challenge
lifecycle vectors, verifies their embedded wire payloads and proofs, pins every
preserved v2 section digest, and requires all published outcomes to remain inert
and non-authorizing.

Signature proof only shows that the key embedded in a structurally valid
envelope signed its exact challenge response. It is never authorization:
runtime callers must separately match the exact binding and challenge against
server-enrolled session and issued challenge records. Parsing Launchplane's
published expected outcomes does not reproduce or replace its DB-backed
verifier or lifecycle transaction behavior. This crate adds no same-UID
isolation, client-path authenticity, gesture-source, key-custody, principal,
credential, route, or live Launchplane adoption claim. No real operator,
tenant, repository, credential, private key, or runtime configuration is stored
here. Published cryptographic material is synthetic conformance data, not a
credential.
