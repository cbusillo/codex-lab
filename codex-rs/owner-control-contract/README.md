# Owner Control Contract

This standalone crate provides the first behavior-neutral Rust representation
of Launchplane's owner-control contract. It is not connected to CLI, core,
TUI, app-server, tools, MCP, Code Bridge, networking, helper processes, key
custody, IPC, runtime authorization, or other runtime behavior.

The vendored artifact at `contracts/owner-control-contract.json` is byte-for-byte
from reviewed `cbusillo/launchplane` head
`bb20d9ae6754c7c408ea275e9a135d39f2cb971d`, merged into `main` by PR `#2275`
as merge commit `6e60897eebd6ee2ba2a3bc234e85de531c8298a0`. Its SHA-256 is
`cf2815b65bafb7e25b00647dbdfd464577cb0a6e8a861ae3e1e019840865804e`.

The public API exposes typed artifact loading, strict model validation, the
artifact's canonical JSON serializer, SHA-256 helpers, and Ed25519 signature
proof verification over published conformance envelopes. The v5 container also
strictly parses Launchplane's synthetic shadow-verification and challenge
lifecycle vectors and enrollment-provenance evidence, verifies their embedded
wire payloads and proofs, recomputes every preserved-v4 digest, and requires all
published provenance combinations to remain `self_asserted`, inert, and
non-authorizing. Caller claims for principal separation, key custody, or gesture
source never raise trust without server-observed corroboration.

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
