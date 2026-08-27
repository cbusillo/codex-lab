# Upstream convergence inventory

- Merge base: `cbfd999db78cb088d2bd89b52051efe6f44555a4`
- Upstream snapshot: `cbfd999db78cb088d2bd89b52051efe6f44555a4`
- Local baseline: `142e61537abb04850b2656747f979c77549aca8c`
- Conflicts: 0
- Residual local-influence paths retained by an upstream-first merge: 1143

Residual paths merge cleanly, so no reviewer sees them. The merge keeps
local content there instead of upstream content; it does not reject it.
`residuals.json` lists every one with its contract lane.

## Counts

| Dimension | Value |
| --- | ---: |
| Residual lane `amber_contract_adapt` | 330 |
| Residual lane `green_bulk_adopt` | 444 |
| Residual lane `intentionally_owned` | 366 |
| Residual lane `red_manual_review` | 3 |

## Contract-reviewed conflicts

Green paths are intentionally omitted from this table because the candidate
takes upstream unchanged. The JSON companion records every conflict path.

| Lane | Contracts | Path | Reason |
| --- | --- | --- | --- |
