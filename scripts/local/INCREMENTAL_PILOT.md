# Incremental cache pilot

This is an opt-in POSIX experiment for macOS and Linux. Set `RUSTC_WRAPPER` to
`scripts/local/incremental-cache-wrapper.py`; do not set
`RUSTC_WORKSPACE_WRAPPER`. The wrapper routes on rustc's actual `-C
incremental=...` or equivalent `--codegen` flag. Response-file invocations go
directly to rustc because the wrapper does not scan response files.

Cargo computes rustc flags before invoking the wrapper. For cache-compatible
calls, the wrapper removes only `CARGO_INCREMENTAL` from the sccache child
environment; direct incremental rustc calls retain the original flags and
environment. Release profile flags and other Cargo settings are preserved.

Run each arm as a separate alternative with a fresh target and isolated
sccache socket/cache:

```sh
export EXTERNAL_PILOT_TARGET=/path/to/isolated/pilot-root

# A: no Cargo incremental state; all compatible rustc calls use sccache.
CARGO_TARGET_DIR="$EXTERNAL_PILOT_TARGET/a-target" \
SCCACHE_SERVER_UDS="$EXTERNAL_PILOT_TARGET/a.sock" \
SCCACHE_DIR="$EXTERNAL_PILOT_TARGET/a-cache" \
  CARGO_INCREMENTAL=0 RUSTC_WRAPPER=sccache just test -p codex-utils-string

# B: Cargo incremental state; no compiler cache wrapper.
CARGO_TARGET_DIR="$EXTERNAL_PILOT_TARGET/b-target" \
  CARGO_INCREMENTAL=1 RUSTC_WRAPPER= just test -p codex-utils-string

# C: retain incremental workspace crates; cache compatible nonincremental calls.
CARGO_TARGET_DIR="$EXTERNAL_PILOT_TARGET/c-target" \
SCCACHE_SERVER_UDS="$EXTERNAL_PILOT_TARGET/c.sock" \
SCCACHE_DIR="$EXTERNAL_PILOT_TARGET/c-cache" \
  CARGO_INCREMENTAL=1 RUSTC_WRAPPER="$PWD/scripts/local/incremental-cache-wrapper.py" \
  just test -p codex-utils-string
```

Explicit target and cache paths are unmanaged overrides; the caller must
validate the external volume, isolation, capacity, and cleanup policy. This
pilot is not an accepted default, performance proof, or GC policy, and makes
no speed claim. Do not restart a shared sccache daemon for it.
