# Upstream convergence evidence

Codex Lab records each upstream-first integration snapshot under
`upstream/openai-codex/<merge-base>-<upstream>/`. The checked-in inventory is
mechanical evidence, not a decision to retain local code.

`upstream/convergence-policy.json` identifies the canonical upstream, evidence
root, contract document, and durable plan. It is deliberately a small discovery
manifest rather than a second implementation of the lane rules below.

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

Historical snapshots remain immutable. They were created with convergence
policy version 1, so reproduce them from the repository root with that version:

```sh
snapshot=upstream/openai-codex/62fd4103-61a44880
base=62fd410384cca008446c2d64a4f2b3f915f4906e
upstream=61a44880a85d2fd0d8770908dea5733495e571c8
local=cd0c1ddbc6b7f92ce0d83cb4db28c6573a25bc59

for format in json markdown residuals; do
  python3 .github/scripts/upstream_convergence_inventory.py "$format" \
    "$base" "$upstream" "$local" --policy-version 1 > "/tmp/$format.out"
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

`upstream/convergence-guard.json` is the durable answer. For every
`intentionally_owned` or `red_manual_review` path it records the baseline blob
and the upstream blob, drawn from two sources:

- `ownership_baseline`: the path already differed from upstream at the pinned
  pre-anchor local baseline.
- `current_tree`: the path is owned in the candidate itself. Owned work created
  or restored *after* the baseline is invisible to the baseline source, so
  without this the manifest had to be hand-edited to protect new proofs, and a
  hand-edited generated artifact drifts silently. Adding a path can only
  increase protection, so this source cannot launder an anchor loss.

Ownership itself comes from path patterns in
`upstream_convergence_inventory.py`, not an enumerated list. Owned features are
declared by filename stem and expanded across the conventional implementation
and integration-proof roots, so an implementation and the proof that pins it are
guarded together. Suite registry modules are guarded as well: reverting one
unregisters every owned proof in that crate while leaving each proof file in
place. That includes the nested `app-server/tests/suite/v2/mod.rs`, because
every Every Code-owned app-server proof is a v2 suite and registers there rather
than in the crate-level `tests/suite/mod.rs`.

Shared upstream modules that carry an owned delta but no owned filename -- the
TUI thread routing module, the context manager history, the hook config and
discovery modules, the turn-context writer -- are listed explicitly by their
contract. Guarding one still only forbids deletion and byte-identical reversion,
never ordinary upstream edits.

`upstream_convergence_guard.py` then fails a candidate when a guarded path is:

- **absent** from the tree, or
- **byte-identical** to the recorded upstream blob.

The two suite entrypoints marked `presence_only` are the deliberate exception:
their upstream bytes are correct, but deleting either file would unregister the
owned test suites it connects to the compiled test binary. Those rows are
therefore checked for existence rather than content divergence.

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

Regenerate the manifest when the ownership baseline advances, when the
classification rules change, or when owned work lands that should be guarded:

```sh
python3 .github/scripts/upstream_convergence_inventory.py guard \
  b89ce9a2bcedcfddf3a48f387b7912d602d6d87c \
  4462b9deef211723b781b426f5e5d36a5777115f \
  8add494682f7c0674672e8dc5b38a4565cd7629b \
  . --current HEAD --policy-version 1 > upstream/convergence-guard.json
```

The three positional refs stay at pre-anchor local `8add4946` and its snapshot
pair on purpose: recomputing that source from the current candidate would bake
the anchor's losses into the contract and make the guard agree with the failure
it exists to catch. `--current` only adds candidate-side owned paths, so it
cannot remove protection. Never hand-edit the manifest; add a pattern rule
instead.

Issue #230 is the durable continuous-maintenance plan.
`upstream/convergence-contracts.md` defines which Codex Lab differences may
survive the upstream-first default. `upstream/convergence-gates.json` is the
machine-readable evidence projection for that matrix. Blocking repo checks
verify that every contract ID retains file-backed evidence, local proof paths
exist, declared symbol text remains present, suite proofs stay registered, and
non-executable release claims name a deciding issue. The recorded CI tiers are
an inventory; they do not imply that nightly or release proof runs on every pull
request. Bootstrap history remains available in the completed issue #428.

## Supported command

Use the phase-specific repository command instead of reconstructing Git
plumbing from memory:

```sh
python3 .github/scripts/upstream_convergence.py inspect \
  --base <full-merge-base-sha> \
  --upstream <full-upstream-sha> \
  --local <full-local-sha>

python3 .github/scripts/upstream_convergence.py record \
  --base <full-merge-base-sha> \
  --upstream <full-upstream-sha> \
  --local <full-local-sha>

python3 .github/scripts/upstream_convergence.py validate \
  --against <full-review-base-sha>
```

`inspect` is read-only, `record` appends one immutable snapshot directory, and
`validate` checks policy, governance wiring, the guard, snapshot structure and
reproducibility, plus any requested review-base comparison. The command never
fetches, merges, builds, commits, pushes, or manages worktrees.

When the exact review base predates `upstream/convergence-policy.json`,
`validate --against` reports `comparisonMode: bootstrap`. Governance, guard,
canonical-remote, complete-history, clean-worktree, and full snapshot
reproducibility checks still run, but append-only and new-snapshot provenance
comparison are not applied retroactively to pre-adoption evidence. A regular
policy file at the review base permanently selects `comparisonMode: strict`;
symlinked or non-file policy entries fail closed.

## Skill coordination

When the routing instruction and shared skill change together, merge and
reconcile the `upstream-convergence` skill first. Verify that
`$upstream-convergence` resolves from the active skills checkout before landing
the Codex Lab commit that requires the route.

Repository checks protect against accidental convergence regressions. GitHub
must separately require blocking CI and code-owner review for the paths in
`.github/CODEOWNERS`; candidate-controlled scripts cannot provide that external
trust boundary by themselves.
