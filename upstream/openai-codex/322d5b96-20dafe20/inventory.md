# Upstream convergence inventory

- Merge base: `322d5b96cfa5c8fd52bd83ecfdb79cd9b330205f`
- Upstream snapshot: `20dafe201d91d4405eef05ecd1db0257f13a9ac8`
- Local baseline: `2d782218d9cac05ade4c0839c21da46295b69c4c`
- Conflicts: 0
- Residual local-influence paths retained by an upstream-first merge: 388

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Residual lane `amber_contract_adapt` | 124 |
| Residual lane `green_bulk_adopt` | 170 |
| Residual lane `intentionally_owned` | 84 |
| Residual lane `red_manual_review` | 10 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
