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
