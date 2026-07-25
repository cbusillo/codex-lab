# Upstream convergence evidence

Codex Lab records each upstream-first integration snapshot under
`upstream/openai-codex/<merge-base>-<upstream>/`. The checked-in inventory is
mechanical evidence, not a decision to retain local code.

Each snapshot contains:

- `inventory.json`: exact refs, merge-conflict types, contract lanes, and every
  conflicted path.
- `inventory.md`: a compact review surface for non-green conflicts.
- `residuals.json`: every non-conflicting path whose merge result differs from
  upstream, with its contract lane.

A residual path is one an upstream-first merge **retains** from local without
showing a reviewer anything. Nothing rejects it, which is why each one needs a
named contract lane. Schema version 2 renamed the misleading
`silentLocalInfluence` summary key to `residualLocalInfluence` for that reason.

Regenerate a snapshot from the repository root:

```sh
snapshot=upstream/openai-codex/322d5b96-20dafe20
base=322d5b96cfa5c8fd52bd83ecfdb79cd9b330205f
upstream=20dafe201d91d4405eef05ecd1db0257f13a9ac8
local=2d782218d9cac05ade4c0839c21da46295b69c4c

for format in json markdown residuals; do
  python3 .github/scripts/upstream_convergence_inventory.py "$format" \
    "$base" "$upstream" "$local" > "/tmp/$format.out"
done
cmp /tmp/json.out "$snapshot/inventory.json"
cmp /tmp/markdown.out "$snapshot/inventory.md"
cmp /tmp/residuals.out "$snapshot/residuals.json"
```

## Refresh guard

Anchor merge `9d2eea2238` recorded local history while taking the upstream
tree. Every Every Code-owned file that upstream did not carry vanished without
one conflict marker, and no later merge can resurrect it because the anchor is
already the merge base. Nothing in CI noticed.

`upstream/convergence-guard.json` is the durable answer. It pins an ownership
baseline and, for every `intentionally_owned` or `red_manual_review` path whose
local blob differed from upstream at that baseline, records the baseline blob
and the upstream blob. `upstream_convergence_guard.py` then fails a candidate
when such a path is:

- **absent** from the tree, or
- **byte-identical** to the recorded upstream blob.

`repo-checks.yml` runs the guard on every pull request, so a refresh that
silently reverts owned behavior cannot merge.

Green and amber lanes never enter the manifest, so ordinary upstream deletions
and upstream rewrites of shared code stay unblocked. When an owned path really
should follow upstream, record the decision in
`upstream/convergence-waivers.json` with an explicit `violation`, a
`disposition`, the deciding `issue`, and a `reason`:

- `upstream_deletion_adopted`: upstream deleted the path and Codex Lab agreed.
- `converged_with_upstream`: upstream adopted the Codex Lab behavior.
- `pending_restore`: the path was lost by an upstream-first merge and restoring
  it is tracked work.

A waiver that no longer matches a violation fails the guard too, so a restored
path cannot leave a dead entry behind.

Regenerate the manifest only when the ownership baseline advances:

```sh
python3 .github/scripts/upstream_convergence_inventory.py guard \
  b89ce9a2bcedcfddf3a48f387b7912d602d6d87c \
  4462b9deef211723b781b426f5e5d36a5777115f \
  8add494682f7c0674672e8dc5b38a4565cd7629b > upstream/convergence-guard.json
```

The baseline stays at pre-anchor local `8add4946` on purpose: regenerating it
from the current candidate would bake the anchor's losses into the contract and
make the guard agree with the failure it exists to catch.

Issue #428 is the durable integration plan. `docs/convergence-contracts.md`
defines which Every Code differences may survive the upstream-first default.
