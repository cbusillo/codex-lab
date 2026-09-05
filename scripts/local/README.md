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
command. Schema v2 omits raw paths, command arguments and flag values. Its stable
fingerprints are pseudonymous, not anonymized: review evidence before publishing. Different path/flag fingerprints can explain
reuse differences and must not be normalized away blindly.

Optional storage telemetry samples once per second and reports observed minimum
free space, not an exact peak or bytes caused by the command. Disk figures are
shared-host measurements; sccache counters are shared-server aggregates. Use
`--concurrent-builds` for uncontrolled overlapping work; such samples are marked
non-comparable. Dirty checkouts require `--allow-dirty` and are also non-comparable.
Failed commands, counter resets and degraded requested storage telemetry are not
comparable. The harness preserves the measured command's exit status.

Compare matching schemas, declared workloads and input identities, accounting for
compiler/link/test/package and cache-transfer stages with separate measurements.
The harness measures total command time; it does not invent those subphase timings.
It never changes cache routing, compilation settings, retention or admission policy.
