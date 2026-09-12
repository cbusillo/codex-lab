# Local build measurements

## Cooperative target ownership

`target_ownership.py run --root ROOT --target NAME -- COMMAND ...` creates a
new target and holds a private sidecar lease. Existing targets remain unsupported.
Nested ownership commands refuse before creating another target; the helper
currently supports one target lease per command tree.
The command receives `CODEX_LAB_OWNED_TARGET` and
`CODEX_LAB_TARGET_LEASE_FD`; callers still choose the command's actual target
routing. This helper does not set `CARGO_TARGET_DIR` automatically.

The lease descriptor survives cooperative child launches and foreground exit.
Closing the supervisor's copy also preserves a descendant's copy after a
supervisor crash. The Python `just` prerequisite-build and source-package Cargo
launchers explicitly forward a configured descriptor and reject invalid ones.
Other subprocess boundaries must preserve the descriptor themselves.

`inspect` reports `lease-held-after-release` when a released claim still has a
lease holder. A free lock remains **unverified**: a detached process can close
its inherited descriptor and still need the target later. Every current claim
state remains protected; this helper does not enable target reuse, automatic
retention, or deletion. Programs that outlive a managed build must retain their
lease or consume independently staged artifacts.

## Managed target retention (macOS, opt-in)

`managed_targets.py` creates a separate private store for newly enrolled Cargo
targets. It never adopts existing `cargo-target` directories or v1 ownership
claims. Each Git worktree gets its own reusable target; mutable targets are
never shared between worktrees. The source identity includes a private nonce in
the worktree's Git directory, so reusing a source path does not reuse its owner.
Use Python 3.11 or newer for collection; older runtimes without safe
directory-descriptor-relative deletion leave targets protected.

Initialize an unused directory on the intended mounted volume:

```sh
python3 scripts/local/managed_targets.py --config /absolute/host-config.json init \
  --volume-root /Volumes/BuildArtifacts --volume-uuid VOLUME-UUID \
  --managed-root /Volumes/BuildArtifacts/managed-cargo
```

Select that configuration with `CODEX_LAB_TARGET_RETENTION_CONFIG`, or use the
default `~/.config/codex-lab/target-retention.json`. With configuration present,
supported `just test`, `clippy`, `fix`, and source `assemble-codex-package`
invocations run under a lease, including their prerequisite builds. Use
`just managed-build` for the explicit build entrypoint. Unknown arguments and
explicit Cargo target/config overrides stay unmanaged; overrides pointing into
the managed store are refused. Raw Cargo, IDE launches, runtime launches, older
checkouts, and other projects are not automatically enrolled.

This is a **cooperative workflow contract**, not a universal process detector.
Every target-dependent child must preserve `CODEX_LAB_TARGET_LEASE_FD`.
Detached programs must retain that lease or consume separate copies outside the
managed store. Source packaging copies its executables out before completing;
custom Cargo/prebuilt inputs and package outputs inside the store are excluded
from managed enrollment. Do not launch programs directly from managed targets.

Kache is started and checked before acquiring the build lease. Managed children
set `KACHE_AUTO_GC=0` so a detached wrapper GC cannot inherit that lease and
delay target reuse. The host configuration stays unchanged: the prestarted
daemon continues its native periodic GC, and collection runs native GC again
after deleting a target. The bare native GC command honors Kache's configured
age and size policies independently of target retention.

Preview with `just managed-target-gc`; pass `--apply` to collect. The default
policy expires eligible targets after 14 days without use. Below 200 GiB free,
it can select an eligible target idle for at least 24 hours. The 24-hour grace
preserves recent warm output; it is not evidence that a process has finished.
Each pass removes at most one whole target, then invokes Kache's native GC and
remeasures available space. This bounds target count, not deletion time or
bytes. Filesystem free space is a snapshot, not a concurrent-build reservation.

Only a completed managed run with matching identities and a free lease can be
collected. Active, interrupted, malformed, replaced, and unknown records remain
protected. A missing source worktree alone is not a deletion permit. Collection
never recovers an interrupted build automatically. A later managed invocation
in that same worktree can resume its target after validating its identity and
acquiring the exclusive lease; an existing lease holder still blocks reuse.
Collection uses the same per-target lease as builds and quarantines the target before
deleting it. Interrupted collection is left protected for inspection; it is not
automatically retried by guessing ownership. An admission attempt below the
floor performs one bounded collection and refuses if space remains insufficient.
A failed initial creation can leave an unregistered directory, which is also
protected and requires operator inspection; the collector does not scan for or
automatically remove such leftovers.

`init` does not install a scheduler. A host scheduler may invoke the `gc --apply`
CLI from a stable, reviewed installation with an explicit configuration and
logs. Keep that installation independent of disposable worktrees, and disable
its scheduled invocation before changing the store protocol. Disabling the
scheduler/configuration preserves all existing target data.

## Optional local storage admission

`scripts/local/cargo-build-env.sh` keeps the existing precedence rules and
accepts two opt-in, host-configured floors:

```sh
CODEX_LAB_STORAGE_MIN_ROOT_FREE_BYTES=1000000000
CODEX_LAB_STORAGE_MIN_TARGET_FREE_BYTES=2000000000
```

Each value must be a positive canonical decimal no larger than `2^63-1`.
Unset values disable that floor; set-but-empty values refuse the command.
Configuring either floor requires Python 3.10 or newer on `PATH`.
Configured floors are checked independently with native filesystem available
space before the target directory is created. A failed configured check stops
the command; it does not fall back to the repository target. Explicit
`CODEX_LAB_CARGO_TARGET_DIR` or `CARGO_TARGET_DIR` values remain honored and
are reported as unmanaged, while configured floors still apply.

These are pre-command capacity snapshots, not reservations or hard quotas;
concurrent work can consume space after admission. Missing target directories
are measured on their nearest existing canonical parent. Configured artifact
roots retain the existing mount-identity checks; an unmanaged override alone
cannot prove that an intended external volume is mounted. Path-resolution
errors refuse admission even when only the root floor is configured.

The maintained `just` Cargo recipes call this resolver. A direct `cargo`
command and a raw IDE Cargo launch bypass it unless the caller explicitly
evaluates the resolver or configures the IDE to invoke `just`; there is no
universal Cargo or IDE interception.

`just local-build-storage --path root=/ --path artifacts="$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT"`
reports filesystem capacity for explicitly named paths without walking or deleting
build data. Paths are represented by caller-selected public-safe role names.
Multiple roles on one filesystem share a filesystem entry; do not sum their free
space. Distinct APFS volumes may also share a container; filesystem totals are
not an aggregate device budget. Missing/inaccessible paths remain unknown, never zero.

Add `--allocated` only for a bounded inventory of specific cache/target paths.
Allocation lookup uses `du` with a timeout, skips symlinks, and reports unavailable
on unsupported platforms or failed scans. Allocation scans stop at device boundaries. Allocated bytes are not guaranteed
reclaimable bytes; shared blocks and concurrent changes affect the result.

For a declared build workload:

```sh
just feedback-latency --lane focused-leaf --scenario warm-noop \
  --configuration dev-default --storage-path root=/ \
  --storage-path artifacts="$CODEX_LAB_DEVELOPER_ARTIFACTS_ROOT" \
  --output /tmp/focused-leaf-01.json -- just test -p codex-utils-string
```

Choose a real package/workload appropriate to the experiment. Use a unique output
file for each sample. `--configuration` is a caller-declared profile/target/features
label; the harness does not infer effective Cargo settings from an arbitrary
command. Schema v4 omits raw paths, command arguments and flag values. Its stable
fingerprints are pseudonymous, not anonymized: review evidence before publishing. Different path/flag fingerprints can explain
reuse differences and must not be normalized away blindly. It also records the
commit, a SHA-256 of the tracked `git diff HEAD`, and whether untracked changes
were present; diff text and paths are never written to evidence.

Schema v4 adds `measurementQuality` for busy-host analysis. The existing
`comparable` field keeps its original meaning, so a run with
`--concurrent-builds` remains non-comparable. `measurementQuality` separates
that declared load condition from integrity: `integrity.status` is `valid` only
when source identity, cache identity, command completion, preflight, build
context, and requested storage telemetry are valid. Its bounded `reasons` list
keeps failures visible. `matchedAnalysisEligible` can still be `true` for a
valid busy-host run, while `load.observedIsolation` remains `unknown`; the
harness does not infer actual isolation from the caller's declaration.
`analysisScope` limits eligibility to command and total duration. It is an
integrity prerequisite for matched latency analysis, not proof of matched
workloads, causal attribution, or comparable cache and disk figures. Cache
counters remain server aggregates and filesystem figures remain shared-host
observations even when latency analysis is eligible.
This eligibility contract currently requires known sccache backend identity.
An explicitly uncached workload needs a separate declared cache-mode contract
before using this field; an ineligible record can still contain useful latency
observations.

For a controlled source edit, compute the declared digest from the checkout and
identify the scenario explicitly:

```sh
repo_root="$(git rev-parse --show-toplevel)"
expected_diff_sha256="$(git -C "$repo_root" diff --no-ext-diff --no-textconv --no-color --binary HEAD -- \
  | shasum -a 256 | awk '{print $1}')"
just feedback-latency --lane focused-leaf-edit --scenario warm-edit \
  --expected-diff-sha256 "$expected_diff_sha256" \
  --output /tmp/focused-leaf-edit-01.json -- just test -p codex-utils-string
```

The declared digest must match the tracked edit before the command starts and
the checkout must have no untracked changes. The harness samples the same
identity after the command; a changed commit, tracked diff, or untracked state
marks the result non-comparable. A dirty checkout without a declared digest
can still be measured with `--allow-dirty`, but remains non-comparable.
The digest is checkout-local: compute it from the same repository immediately
before invoking the harness and do not treat equal or different values across
checkouts as portable edit identities.
This endpoint check cannot detect a transient edit that is reverted before the
post-command scan.

Optional storage telemetry samples once per second and reports observed minimum
free space, not an exact peak or bytes caused by the command. Disk figures are
shared-host measurements; sccache counters are server aggregates. Use
`--concurrent-builds` for uncontrolled overlapping work; such samples are marked
non-comparable. Dirty checkouts require `--allow-dirty` and are also non-comparable
unless they use the controlled-edit digest described above.
Failed commands, counter resets and degraded requested storage telemetry are not
comparable. A known sccache backend identity change is also non-comparable;
unknown backend identity remains explicit in the evidence. Matching identity is
not proof that the same daemon process instance continued running. The harness
preserves the measured command's exit status.

Compare matching schemas, declared workloads and input identities, accounting for
compiler/link/test/package and cache-transfer stages with separate measurements.
The harness measures total command time; it does not invent those subphase timings.
It never changes cache routing, compilation settings, retention or admission policy.
