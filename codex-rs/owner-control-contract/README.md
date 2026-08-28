# Owner Control Contract

This standalone crate provides the first behavior-neutral Rust representation
of Launchplane's owner-control contract. It is not connected to CLI, core,
TUI, app-server, tools, MCP, Code Bridge, networking, helper processes, key
custody, IPC, runtime authorization, or other runtime behavior.

The vendored artifact at `contracts/owner-control-contract.json` is byte-for-byte
from `cbusillo/launchplane` commit
`932aff66ec1317f21cf18697a509dd1497751db5`, merged into `main` by PR `#2261`
as merge commit `8c34cb5849edafd8db05f936afe994ac82372087`. Its SHA-256 is
`342a07917bdfc1a0f4ee43e6ec2b55adebf301b2abfcdab3aa979ce38cf92cc5`.

The public API exposes typed artifact loading, strict model validation, the
artifact's canonical JSON serializer, SHA-256 helpers, and Ed25519 signature
proof verification over published conformance envelopes. Signature proof only
shows that the key embedded in a structurally valid envelope signed its exact
challenge response. It is never authorization: runtime callers must separately
match the exact binding and challenge against server-enrolled session and issued
challenge records. No real operator, tenant, repository, credential, private
key, or runtime configuration is stored here. Published cryptographic material
is synthetic conformance data, not a credential.
