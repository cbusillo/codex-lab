# Canonical local Rust CI temp

Allocates one fresh private directory directly under the external volume
containing `RUNNER_TEMP`, recording its path, device, inode, owner, and volume
identity. Post removes only that directory after revalidating every identity.

Hard-killed or power-lost jobs may leave an orphan because post does not run.
Failed cleanup retains unverified directories, reports their path, and fails the job.
There is deliberately no stale sweep or automatic garbage collection; orphan
cleanup requires an owner-approved exact-path operation.
