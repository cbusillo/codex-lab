# Upstream convergence inventory

- Merge base: `20dafe201d91d4405eef05ecd1db0257f13a9ac8`
- Upstream snapshot: `62fd410384cca008446c2d64a4f2b3f915f4906e`
- Local baseline: `ced83ba965e65a64d9816ebbd4b861c7d91af1f9`
- Conflicts: 0
- Residual local-influence paths retained by an upstream-first merge: 445

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Residual lane `amber_contract_adapt` | 148 |
| Residual lane `green_bulk_adopt` | 193 |
| Residual lane `intentionally_owned` | 94 |
| Residual lane `red_manual_review` | 10 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
