# Upstream convergence inventory

- Merge base: `62fd410384cca008446c2d64a4f2b3f915f4906e`
- Upstream snapshot: `61a44880a85d2fd0d8770908dea5733495e571c8`
- Local baseline: `cd0c1ddbc6b7f92ce0d83cb4db28c6573a25bc59`
- Conflicts: 1
- Residual local-influence paths retained by an upstream-first merge: 446

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Conflict `content` | 1 |
| Lane `amber_contract_adapt` | 1 |
| Residual lane `amber_contract_adapt` | 146 |
| Residual lane `green_bulk_adopt` | 192 |
| Residual lane `intentionally_owned` | 98 |
| Residual lane `red_manual_review` | 10 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
| `amber_contract_adapt` | `PROTOCOL-1` | `codex-rs/app-server/tests/suite/v2/thread_fork.rs` | app-server and wire compatibility |
