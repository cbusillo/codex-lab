# Busy-host A/A analysis

`busy_host_baseline.py` analyzes a preregistered manifest. The execution driver owns preparation, edits, targets, cache endpoints, and the harness; this module never starts commands, waits for quiet, changes Git, or restarts services.

```sh
uv run python scripts/local/busy_host_baseline.py \
  --manifest /path/aa-measured/manifest.json \
  --evidence-root /path/aa-measured \
  --lane aa-busy --configuration dev \
  --output /path/aa-measured/analysis.json
```

The manifest is bounded to 40 attempts. Evidence paths must be relative and remain under the root; unsafe paths, symlinks, invalid manifest or identity structure, and existing output are rejected. Missing, oversized, nonregular, or unparsable per-attempt evidence remains counted as failed evidence. Schema-4 records need exact `matchedAnalysisEligible == true`, a successful command, and positive `phaseDurationsMs.command`.

Pairs require distinct labels, order indexes 0/1, and matching source, edit, toolchain/lockfile, invocation, source-path, configuration, and scenario identities. Environment fingerprints remain stable within each replica and are retained but excluded from cross-replica identity matching: separate targets/caches are caller-declared treatment, not proven equivalent by hashes. Ratios use sorted labels.

Results report paired log ratios, median absolute noise, complete/incomplete pairs, and the worst pair. There are no cache, storage, significance, winner, or A/B/C performance claims.

Collection must also have bounded overhead. Use known source/log paths and filesystem point counters; do not recursively search or inventory live runner, sandbox, target, or cache trees. Limit any allocation traversal to explicitly owned measurement targets with a declared time budget. Piping a search to `head` limits printed lines but does not bound the traversal needed to produce them. Retain and supervise every background command's session identifier; stop only owned diagnostic processes when their budget expires, never build processes. Record accidental diagnostic load as measurement interference.
