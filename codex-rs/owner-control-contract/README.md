# Owner Control Contract

This standalone crate provides the first behavior-neutral Rust representation
of Launchplane's owner-control contract. It is not connected to CLI, core,
TUI, app-server, tools, MCP, Code Bridge, networking, custody, IPC, or runtime
behavior.

The vendored artifact at `contracts/owner-control-contract.json` is byte-for-byte
from `cbusillo/launchplane` commit
`3fed906b9107aafca92026ce50fa28965dde7cf9`, merged into `main` by PR `#2260`
as merge commit `a5018f8ca5befc7c25e72ef5e61c755db1e3fb46`. Its SHA-256 is
`b4ce407a5cfdfb8336924db5a0ab4b887b701ebb76fcb36d8577250d0899e064`.

The public API exposes typed artifact loading, strict model validation, the
artifact's canonical JSON serializer, and SHA-256 helpers. No real operator,
tenant, repository, credential, or runtime configuration is stored here.
