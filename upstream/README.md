# Upstream Evidence

Codex Lab tracks OpenAI Codex updates as immutable audit waves. Adding an audit
or ledger records evidence and decisions; it does not import upstream code.

Each wave lives under `upstream/openai-codex/<after>-<through>/` and contains:

- `audit.json`: mechanical commit and patch-equivalence evidence pinned to one
  implementation baseline, classified checkpoint, and observed upstream head.
- `ledger.json`: the validated semantic disposition for every commit in the
  selected pre- or post-checkpoint window.
- `review.json`: reviewer confidence, rationale, source-review attribution,
  dependency edges, and reviewer notes. Dependency edges use
  `from`, `dependsOn`, and `reason` fields.
- `ledger.md`: deterministic review rendering generated from the audit and
  ledger.

Mechanical states such as `missing_patch` are not semantic judgments. A commit
can still be implemented differently, intentionally inapplicable, rejected, or
blocked on an Every Code product decision. Likewise, a post-checkpoint ledger
does not prove that earlier applicable commits are complete; the pre-checkpoint
range is tracked separately in issue #407.

## Validation

From the repository root, validate a wave and reproduce its Markdown rendering:

```sh
wave=upstream/openai-codex/1bbdb327-bd9a28a8
python3 scripts/github/upstream_semantic_ledger.py validate \
  --audit "$wave/audit.json" \
  --ledger "$wave/ledger.json"
python3 scripts/github/upstream_semantic_ledger.py render \
  --audit "$wave/audit.json" \
  --ledger "$wave/ledger.json" > /tmp/upstream-ledger.md
cmp /tmp/upstream-ledger.md "$wave/ledger.md"
```

Use `--require-complete` only when a wave must have no `decision_required` or
`unclear` commits. A ledger can have exact coverage while still reporting
explicit product-decision blockers.

## Current Wave

The `1bbdb327-bd9a28a8` wave is tracked by issue #408. Its unresolved product
decisions are split into #409 (Bedrock transport), #410 (Max/Ultra TUI), #411
(multi-agent v2 readiness), and #412 (R2 release publication). New upstream
commits after `bd9a28a839d3dc4cf1facdf66cd02bb5732189e3` belong to a later wave rather
than changing this range in place.
