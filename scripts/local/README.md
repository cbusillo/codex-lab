# Local build measurements

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
command. Schema v3 omits raw paths, command arguments and flag values. Its stable
fingerprints are pseudonymous, not anonymized: review evidence before publishing. Different path/flag fingerprints can explain
reuse differences and must not be normalized away blindly. It also records the
commit, a SHA-256 of the tracked `git diff HEAD`, and whether untracked changes
were present; diff text and paths are never written to evidence.

For a controlled source edit, compute the declared digest from the checkout and
identify the scenario explicitly:

```sh
expected_diff_sha256="$(git diff --no-ext-diff --no-textconv --no-color --binary HEAD -- \
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
