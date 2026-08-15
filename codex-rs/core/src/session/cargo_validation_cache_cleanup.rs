use std::fs::File;
use std::io;
use std::path::Path;
use std::time::Instant;

use fs2::FileExt;

use super::CACHE_LOCK_RETRY;
use super::CACHE_LOCK_TIMEOUT;
use super::CargoValidationCacheKey;
use super::CargoValidationCacheLimits;
use super::cleanup_pending_path;
use super::clear_cleanup_pending;
use super::combine_with_unlock;
use super::mark_cleanup_pending;
use super::open_lock;
use super::prune_cache_locked;

const CACHE_CLEANUP_HANDOFF_LOCK_FILE: &str = "cleanup.handoff.lock";

pub(super) fn enforce_cache_bounds(
    root: &Path,
    key: &CargoValidationCacheKey,
    entry_lock: File,
    limits: CargoValidationCacheLimits,
) -> io::Result<()> {
    let cleanup_lock = open_lock(&root.join("cleanup.lock"))?;
    let deadline = Instant::now() + CACHE_LOCK_TIMEOUT;
    loop {
        match cleanup_lock.try_lock_exclusive() {
            Ok(()) => {
                return run_cache_cleanup_locked(root, key, entry_lock, cleanup_lock, limits);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let now = Instant::now();
                if now >= deadline {
                    return hand_off_or_run_cache_cleanup(
                        root,
                        key,
                        entry_lock,
                        cleanup_lock,
                        limits,
                    );
                }
                std::thread::sleep(CACHE_LOCK_RETRY.min(deadline.saturating_duration_since(now)));
            }
            Err(error) => return Err(error),
        }
    }
}

fn hand_off_or_run_cache_cleanup(
    root: &Path,
    key: &CargoValidationCacheKey,
    entry_lock: File,
    cleanup_lock: File,
    limits: CargoValidationCacheLimits,
) -> io::Result<()> {
    let handoff_lock = open_lock(&root.join(CACHE_CLEANUP_HANDOFF_LOCK_FILE))?;
    if !try_lock_until(&handoff_lock, Instant::now() + CACHE_LOCK_TIMEOUT)? {
        if try_lock_until(&cleanup_lock, Instant::now() + CACHE_LOCK_TIMEOUT)? {
            return run_cache_cleanup_locked(root, key, entry_lock, cleanup_lock, limits);
        }
        drop(entry_lock);
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out reacquiring cargo validation cache cleanup ownership",
        ));
    }
    if let Err(error) = mark_cleanup_pending(root) {
        drop(entry_lock);
        return combine_with_unlock(
            Err(error),
            &handoff_lock,
            "cargo validation cache cleanup handoff lock",
        );
    }
    match cleanup_lock.try_lock_exclusive() {
        Ok(()) => {
            if let Err(error) = clear_cleanup_pending(root) {
                drop(entry_lock);
                let result = combine_with_unlock(
                    Err(error),
                    &cleanup_lock,
                    "cargo validation cache cleanup lock",
                );
                return combine_with_unlock(
                    result,
                    &handoff_lock,
                    "cargo validation cache cleanup handoff lock",
                );
            }
            if let Err(error) = FileExt::unlock(&handoff_lock) {
                drop(entry_lock);
                return combine_with_unlock(
                    Err(error),
                    &cleanup_lock,
                    "cargo validation cache cleanup lock",
                );
            }
            run_cache_cleanup_locked(root, key, entry_lock, cleanup_lock, limits)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            // Publish the handoff before releasing this lock, and release the
            // entry before the active cleaner can consume it. The cleaner's
            // final pending check uses the same handoff lock, so it either
            // observes this request or releases cleanup ownership first.
            drop(entry_lock);
            combine_with_unlock(
                Ok(()),
                &handoff_lock,
                "cargo validation cache cleanup handoff lock",
            )
        }
        Err(error) => {
            drop(entry_lock);
            combine_with_unlock(
                Err(error),
                &handoff_lock,
                "cargo validation cache cleanup handoff lock",
            )
        }
    }
}

fn run_cache_cleanup_locked(
    root: &Path,
    key: &CargoValidationCacheKey,
    entry_lock: File,
    cleanup_lock: File,
    limits: CargoValidationCacheLimits,
) -> io::Result<()> {
    let mut entry_lock = Some(entry_lock);
    loop {
        let pass_result = prune_cache_locked(root, key, limits);
        match finish_cache_cleanup_pass(root, &mut entry_lock, &cleanup_lock, pass_result)? {
            CacheCleanupPass::Repeat => {}
            CacheCleanupPass::Complete => return Ok(()),
        }
    }
}

pub(super) enum CacheCleanupPass {
    Repeat,
    Complete,
}

pub(super) fn finish_cache_cleanup_pass(
    root: &Path,
    entry_lock: &mut Option<File>,
    cleanup_lock: &File,
    pass_result: io::Result<()>,
) -> io::Result<CacheCleanupPass> {
    let handoff_lock = open_lock(&root.join(CACHE_CLEANUP_HANDOFF_LOCK_FILE))?;
    if !try_lock_until(&handoff_lock, Instant::now() + CACHE_LOCK_TIMEOUT)? {
        drop(entry_lock.take());
        return combine_with_unlock(
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out acquiring cargo validation cache cleanup handoff lock",
            )),
            cleanup_lock,
            "cargo validation cache cleanup lock",
        );
    }
    let pending_result = cleanup_pending_path(root).try_exists();
    match (pass_result, pending_result) {
        (Ok(()), Ok(true)) => {
            if let Err(error) = clear_cleanup_pending(root) {
                return complete_cache_cleanup(entry_lock, cleanup_lock, &handoff_lock, Err(error));
            }
            match FileExt::unlock(&handoff_lock) {
                Ok(()) => Ok(CacheCleanupPass::Repeat),
                Err(error) => {
                    drop(entry_lock.take());
                    combine_with_unlock(
                        Err(error),
                        cleanup_lock,
                        "cargo validation cache cleanup lock",
                    )
                }
            }
        }
        (Ok(()), Ok(false)) => {
            complete_cache_cleanup(entry_lock, cleanup_lock, &handoff_lock, Ok(()))
        }
        (Err(error), _) | (Ok(()), Err(error)) => {
            complete_cache_cleanup(entry_lock, cleanup_lock, &handoff_lock, Err(error))
        }
    }
}

fn complete_cache_cleanup(
    entry_lock: &mut Option<File>,
    cleanup_lock: &File,
    handoff_lock: &File,
    result: io::Result<()>,
) -> io::Result<CacheCleanupPass> {
    // Release the current entry before cleanup ownership. A timed-out waiter
    // holding the handoff lock can then acquire cleanup ownership without
    // observing this entry as active.
    drop(entry_lock.take());
    let result = combine_with_unlock(result, cleanup_lock, "cargo validation cache cleanup lock")
        .map(|()| CacheCleanupPass::Complete);
    combine_with_unlock(
        result,
        handoff_lock,
        "cargo validation cache cleanup handoff lock",
    )
}

fn try_lock_until(lock: &File, deadline: Instant) -> io::Result<bool> {
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(false);
                }
                std::thread::sleep(CACHE_LOCK_RETRY.min(deadline.saturating_duration_since(now)));
            }
            Err(error) => return Err(error),
        }
    }
}
