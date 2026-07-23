# Upstream convergence evidence

Codex Lab records each upstream-first integration snapshot under
`upstream/openai-codex/<merge-base>-<upstream>/`. The checked-in inventory is
mechanical evidence, not a decision to retain local code.

Each snapshot contains:

- `inventory.json`: exact refs, merge-conflict types, contract lanes, and every
  conflicted path.
- `inventory.md`: a compact review surface for non-green conflicts.

Regenerate the current snapshot from the repository root:

```sh
snapshot=upstream/openai-codex/b89ce9a2-4462b9de
base=b89ce9a2bcedcfddf3a48f387b7912d602d6d87c
upstream=4462b9deef211723b781b426f5e5d36a5777115f
local=8add494682f7c0674672e8dc5b38a4565cd7629b

python3 .github/scripts/upstream_convergence_inventory.py json \
  "$base" "$upstream" "$local" > /tmp/inventory.json
python3 .github/scripts/upstream_convergence_inventory.py markdown \
  "$base" "$upstream" "$local" > /tmp/inventory.md
cmp /tmp/inventory.json "$snapshot/inventory.json"
cmp /tmp/inventory.md "$snapshot/inventory.md"
```

Issue #428 is the durable integration plan. `docs/convergence-contracts.md`
defines which Every Code differences may survive the upstream-first default.
