# Upstream convergence inventory

- Merge base: `ec9620c231396895194329c410f3ec360b4cadef`
- Upstream snapshot: `ec9620c231396895194329c410f3ec360b4cadef`
- Local baseline: `5298acd70a0d3144b0e6c68ab5286a38d8ccb8ac`
- Conflicts: 0
- Residual local-influence paths retained by an upstream-first merge: 1253

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Residual lane `amber_contract_adapt` | 334 |
| Residual lane `green_bulk_adopt` | 543 |
| Residual lane `intentionally_owned` | 373 |
| Residual lane `red_manual_review` | 3 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
