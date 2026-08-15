use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_utils_absolute_path::AbsolutePathBuf;
use fs2::FileExt;
use tokio::sync::Semaphore;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::cargo_validation_cache_key::CACHE_VERSION;
use super::cargo_validation_cache_key::CargoValidationCacheKey;

#[path = "cargo_validation_cache_cleanup.rs"]
mod cleanup;

use cleanup::enforce_cache_bounds;

const CACHE_LOCK_RETRY: Duration = Duration::from_millis(25);
const CACHE_LOCK_TIMEOUT: Duration = Duration::from_millis(250);
const CACHE_PREPARE_TIMEOUT: Duration = Duration::from_millis(500);
const CACHE_CLEANUP_PENDING_FILE: &str = "cleanup.pending";
static CACHE_PREPARE_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(1)));

#[derive(Clone, Copy)]
struct CargoValidationCacheLimits {
    max_entries: usize,
    max_entry_bytes: u64,
    max_total_bytes: u64,
    max_files_per_entry: usize,
}

const CACHE_LIMITS: CargoValidationCacheLimits = CargoValidationCacheLimits {
    max_entries: 4,
    max_entry_bytes: 12 * 1024 * 1024 * 1024,
    max_total_bytes: 24 * 1024 * 1024 * 1024,
    max_files_per_entry: 250_000,
};

pub(crate) struct CargoValidationCacheLease {
    root: PathBuf,
    key: CargoValidationCacheKey,
    target_dir: AbsolutePathBuf,
    entry_lock: File,
    limits: CargoValidationCacheLimits,
}

impl CargoValidationCacheLease {
    pub(crate) async fn acquire(
        codex_home: &AbsolutePathBuf,
        repository_root: &Path,
        checkout_root: &Path,
        key: CargoValidationCacheKey,
        cancellation: &CancellationToken,
    ) -> io::Result<Option<Self>> {
        Self::acquire_with_limits(
            codex_home,
            repository_root,
            checkout_root,
            key,
            cancellation,
            CACHE_LIMITS,
        )
        .await
    }

    async fn acquire_with_limits(
        codex_home: &AbsolutePathBuf,
        repository_root: &Path,
        checkout_root: &Path,
        key: CargoValidationCacheKey,
        cancellation: &CancellationToken,
        limits: CargoValidationCacheLimits,
    ) -> io::Result<Option<Self>> {
        let codex_home = codex_home.clone();
        let repository_root = repository_root.to_path_buf();
        let checkout_root = checkout_root.to_path_buf();
        let Some(root) = bounded_blocking(cancellation, "prepare cache root", move || {
            cache_root(&codex_home, &repository_root, &checkout_root)
        })
        .await?
        else {
            return Ok(None);
        };
        let Some(entry_lock) = acquire_entry_lock(&root, &key, cancellation).await? else {
            return Ok(None);
        };
        let root_for_prepare = root.clone();
        let key_for_prepare = key.clone();
        let Some((target_dir, entry_lock)) =
            bounded_blocking(cancellation, "prepare cache entry", move || {
                prepare_entry(&root_for_prepare, &key_for_prepare, entry_lock)
            })
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            root,
            key,
            target_dir,
            entry_lock,
            limits,
        }))
    }

    pub(crate) fn target_dir(&self) -> &AbsolutePathBuf {
        &self.target_dir
    }

    pub(crate) fn finish(self) {
        spawn_maintenance(self, |result| {
            if let Err(error) = result {
                tracing::warn!(%error, "cargo validation cache maintenance failed");
            }
        });
    }
}

fn spawn_maintenance<F>(lease: CargoValidationCacheLease, report: F)
where
    F: FnOnce(io::Result<()>) + Send + 'static,
{
    if let Err(error) = std::thread::Builder::new()
        .name("cargo-validation-cache-maintenance".to_string())
        .spawn(move || report(maintain_cache(lease)))
    {
        tracing::warn!(%error, "failed to spawn cargo validation cache maintenance");
    }
}

async fn bounded_blocking<T, F>(
    cancellation: &CancellationToken,
    operation: &'static str,
    task: F,
) -> io::Result<Option<T>>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    let deadline = tokio::time::Instant::now() + CACHE_PREPARE_TIMEOUT;
    let permit = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Ok(None),
        permit = tokio::time::timeout_at(
            deadline,
            Arc::clone(&CACHE_PREPARE_SEMAPHORE).acquire_owned(),
        ) => match permit {
            Ok(Ok(permit)) => permit,
            Ok(Err(error)) => {
                return Err(io::Error::other(format!(
                    "cargo cache {operation} worker unavailable: {error}"
                )));
            }
            Err(_) => return Ok(None),
        }
    };
    let (result_tx, result_rx) = oneshot::channel();
    std::thread::Builder::new()
        .name(format!("cargo-cache-{operation}"))
        .spawn(move || {
            let _permit = permit;
            let _ = result_tx.send(task());
        })
        .map_err(|error| {
            io::Error::other(format!(
                "failed to spawn cargo cache {operation} worker: {error}"
            ))
        })?;
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Ok(None),
        result = tokio::time::timeout_at(deadline, result_rx) => match result {
            Ok(Ok(result)) => result.map(Some),
            Ok(Err(error)) => Err(io::Error::other(format!(
                "cargo cache {operation} worker failed: {error}"
            ))),
            Err(_) => Ok(None),
        }
    }
}

fn prepare_entry(
    root: &Path,
    key: &CargoValidationCacheKey,
    entry_lock: File,
) -> io::Result<(AbsolutePathBuf, File)> {
    let entry = entry_path(root, key);
    ensure_directory(&entry)?;
    let target = entry.join("target");
    ensure_directory(&target)?;
    let canonical_target = dunce::canonicalize(&target)?;
    if !canonical_target.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cargo validation cache target resolves outside its trusted cache root",
        ));
    }
    let target_dir = AbsolutePathBuf::try_from(canonical_target).map_err(io::Error::other)?;
    Ok((target_dir, entry_lock))
}

fn cache_root(
    codex_home: &AbsolutePathBuf,
    repository_root: &Path,
    checkout_root: &Path,
) -> io::Result<PathBuf> {
    let trust_roots = [
        dunce::canonicalize(repository_root)?,
        dunce::canonicalize(checkout_root)?,
    ];
    let mut root = dunce::canonicalize(codex_home.as_ref())?;
    ensure_cache_path_outside_trust_roots(&root, &trust_roots)?;
    for component in ["cache", "cargo-validation", CACHE_VERSION] {
        root.push(component);
        ensure_directory(&root)?;
        root = dunce::canonicalize(&root)?;
        ensure_cache_path_outside_trust_roots(&root, &trust_roots)?;
    }
    for directory in [root.join("entries"), root.join("locks")] {
        ensure_directory(&directory)?;
    }
    Ok(root)
}

fn ensure_cache_path_outside_trust_roots(path: &Path, trust_roots: &[PathBuf]) -> io::Result<()> {
    if trust_roots.iter().any(|root| path.starts_with(root)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cargo validation cache must be outside the repository and concrete checkout",
        ));
    }
    Ok(())
}

async fn acquire_entry_lock(
    root: &Path,
    key: &CargoValidationCacheKey,
    cancellation: &CancellationToken,
) -> io::Result<Option<File>> {
    let deadline = tokio::time::Instant::now() + CACHE_LOCK_TIMEOUT;
    loop {
        if cancellation.is_cancelled() {
            return Ok(None);
        }
        let root = root.to_path_buf();
        let key = key.clone();
        let attempt = tokio::task::spawn_blocking(move || try_acquire_entry_lock(&root, &key));
        let entry_lock = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(None),
            result = tokio::time::timeout_at(deadline, attempt) => match result {
                Ok(result) => result
                    .map_err(|error| io::Error::other(format!("cargo cache lock task failed: {error}")))??,
                Err(_) => return Ok(None),
            }
        };
        if entry_lock.is_some() {
            return Ok(entry_lock);
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Ok(None),
            _ = tokio::time::sleep_until(std::cmp::min(deadline, now + CACHE_LOCK_RETRY)) => {}
        }
    }
}

fn try_acquire_entry_lock(root: &Path, key: &CargoValidationCacheKey) -> io::Result<Option<File>> {
    let cleanup_lock = open_lock(&root.join("cleanup.lock"))?;
    match cleanup_lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(error),
    }
    let entry_lock = open_lock(&lock_path(root, key))?;
    let result = match entry_lock.try_lock_exclusive() {
        Ok(()) => Ok(Some(entry_lock)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    };
    combine_with_unlock(result, &cleanup_lock, "cargo validation cache cleanup lock")
}

fn maintain_cache(lease: CargoValidationCacheLease) -> io::Result<()> {
    let CargoValidationCacheLease {
        root,
        key,
        entry_lock,
        limits,
        ..
    } = lease;
    let metadata_result = write_last_used(&entry_path(&root, &key), now_millis());
    // Maintenance runs on a detached blocking task after validation completes.
    // A bounded cleanup-lock retry lets overlapping finish tasks serialize
    // without delaying or replacing the validation result indefinitely.
    let cleanup_result = enforce_cache_bounds(&root, &key, entry_lock, limits);
    combine_maintenance_results(metadata_result, cleanup_result)
}

fn cleanup_pending_path(root: &Path) -> PathBuf {
    root.join(CACHE_CLEANUP_PENDING_FILE)
}

fn mark_cleanup_pending(root: &Path) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(cleanup_pending_path(root))
        .map(|_| ())
}

fn clear_cleanup_pending(root: &Path) -> io::Result<()> {
    match fs::remove_file(cleanup_pending_path(root)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn combine_maintenance_results(
    metadata_result: io::Result<()>,
    cleanup_result: io::Result<()>,
) -> io::Result<()> {
    match (metadata_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(metadata_error), Err(cleanup_error)) => Err(io::Error::other(format!(
            "failed to update cargo validation cache metadata: {metadata_error}; additionally failed to enforce cache bounds: {cleanup_error}"
        ))),
    }
}

fn prune_cache_locked(
    root: &Path,
    current_key: &CargoValidationCacheKey,
    limits: CargoValidationCacheLimits,
) -> io::Result<()> {
    let entries_root = root.join("entries");
    let current = current_key.digest();
    let mut entries = Vec::new();
    for entry in fs::read_dir(&entries_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().into_owned();
        let entry_path = entry.path();
        let measurement_lock =
            if key == current || lock_path(root, current_key) == lock_path_for_digest(root, &key) {
                None
            } else {
                let lock = open_lock(&lock_path_for_digest(root, &key))?;
                match lock.try_lock_exclusive() {
                    Ok(()) => Some(lock),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        entries.push(CacheEntryUsage {
                            key,
                            path: entry_path,
                            bytes: 0,
                            file_limit_exceeded: false,
                            last_used: 0,
                            blocked: true,
                            present: true,
                        });
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            };
        let usage = measure_target(&entry_path.join("target"), limits)?;
        let last_used = read_last_used(&entry_path).unwrap_or_default();
        if let Some(lock) = measurement_lock {
            FileExt::unlock(&lock)?;
        }
        entries.push(CacheEntryUsage {
            key,
            path: entry_path,
            bytes: usage.bytes,
            file_limit_exceeded: usage.file_limit_exceeded,
            last_used,
            blocked: false,
            present: true,
        });
    }

    if let Some(entry) = entries.iter_mut().find(|entry| entry.key == current)
        && (entry.bytes > limits.max_entry_bytes || entry.file_limit_exceeded)
    {
        clear_target(&entry.path)?;
        entry.bytes = 0;
        entry.file_limit_exceeded = false;
    }

    entries.sort_by_key(|entry| (entry.last_used, entry.key.clone()));
    loop {
        let total_bytes = entries
            .iter()
            .fold(0u64, |total, entry| total.saturating_add(entry.bytes));
        let retained_entries = entries.iter().filter(|entry| entry.present).count();
        let has_oversized_entry = entries.iter().any(|entry| {
            entry.present && (entry.bytes > limits.max_entry_bytes || entry.file_limit_exceeded)
        });
        if total_bytes <= limits.max_total_bytes
            && retained_entries <= limits.max_entries
            && !has_oversized_entry
        {
            return Ok(());
        }
        let index = entries
            .iter()
            .position(|entry| {
                entry.key != current
                    && entry.present
                    && !entry.blocked
                    && (entry.bytes > limits.max_entry_bytes || entry.file_limit_exceeded)
            })
            .or_else(|| {
                entries
                    .iter()
                    .position(|entry| entry.key != current && entry.present && !entry.blocked)
            });
        let Some(index) = index else {
            if total_bytes > limits.max_total_bytes
                && let Some(entry) = entries
                    .iter_mut()
                    .find(|entry| entry.key == current && entry.bytes > 0)
            {
                clear_target(&entry.path)?;
                entry.bytes = 0;
                continue;
            }
            // Active entries retain their shard locks and run this same bounded
            // cleanup when they finish, so defer bounds that cannot be enforced
            // without traversing or deleting a concurrently mutating target.
            if entries.iter().any(|entry| entry.present && entry.blocked) {
                return Ok(());
            }
            return Err(io::Error::other(
                "cargo validation cache could not enforce its disk bound while entries were active",
            ));
        };
        let candidate = &entries[index];
        if try_evict(root, current_key, candidate)? {
            entries[index].bytes = 0;
            entries[index].present = false;
        } else {
            entries[index].blocked = true;
        }
    }
}

fn try_evict(
    root: &Path,
    current_key: &CargoValidationCacheKey,
    candidate: &CacheEntryUsage,
) -> io::Result<bool> {
    // Holding the current key's shard lock excludes every other active entry in
    // that shard, so a same-shard candidate is safe to remove without relocking.
    if lock_path(root, current_key) == lock_path_for_digest(root, &candidate.key) {
        remove_entry(&candidate.path)?;
        return Ok(true);
    }
    let lock = open_lock(&lock_path_for_digest(root, &candidate.key))?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
        Err(error) => return Err(error),
    }
    combine_with_unlock(
        remove_entry(&candidate.path).map(|()| true),
        &lock,
        "cargo validation cache entry lock",
    )
}

fn combine_with_unlock<T>(result: io::Result<T>, lock: &File, label: &str) -> io::Result<T> {
    match (result, FileExt::unlock(lock)) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(unlock_error)) => Err(unlock_error),
        (Err(error), Err(unlock_error)) => Err(io::Error::other(format!(
            "{error}; additionally failed to unlock {label}: {unlock_error}"
        ))),
    }
}

fn remove_entry(entry: &Path) -> io::Result<()> {
    match fs::remove_dir_all(entry) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn clear_target(entry: &Path) -> io::Result<()> {
    let target = entry.join("target");
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    ensure_directory(&target)
}

struct CacheEntryUsage {
    key: String,
    path: PathBuf,
    bytes: u64,
    file_limit_exceeded: bool,
    last_used: u64,
    blocked: bool,
    present: bool,
}

struct TargetUsage {
    bytes: u64,
    file_limit_exceeded: bool,
}

fn measure_target(target: &Path, limits: CargoValidationCacheLimits) -> io::Result<TargetUsage> {
    if !target.exists() {
        return Ok(TargetUsage {
            bytes: 0,
            file_limit_exceeded: false,
        });
    }
    let mut bytes = 0u64;
    let mut entries = 0usize;
    let mut pending = vec![target.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            entries = entries.saturating_add(1);
            if entries > limits.max_files_per_entry {
                return Ok(TargetUsage {
                    bytes,
                    file_limit_exceeded: true,
                });
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                pending.push(entry.path());
                continue;
            }
            bytes = bytes.saturating_add(metadata.len());
            if bytes > limits.max_entry_bytes {
                break;
            }
        }
        if bytes > limits.max_entry_bytes {
            break;
        }
    }
    Ok(TargetUsage {
        bytes,
        file_limit_exceeded: false,
    })
}

fn ensure_directory(path: &Path) -> io::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cargo validation cache directory must not be a symlink: {}",
                path.display()
            ),
        ));
    }
    fs::create_dir_all(path)
}

fn open_lock(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

fn entry_path(root: &Path, key: &CargoValidationCacheKey) -> PathBuf {
    root.join("entries").join(key.digest())
}

fn lock_path(root: &Path, key: &CargoValidationCacheKey) -> PathBuf {
    lock_path_for_digest(root, key.digest())
}

fn lock_path_for_digest(root: &Path, digest: &str) -> PathBuf {
    let shard = digest.get(..2).unwrap_or("00");
    root.join("locks").join(format!("{shard}.lock"))
}

fn write_last_used(entry: &Path, timestamp: u64) -> io::Result<()> {
    fs::write(entry.join("last-used"), timestamp.to_string())
}

fn read_last_used(entry: &Path) -> io::Result<u64> {
    fs::read_to_string(entry.join("last-used"))?
        .parse()
        .map_err(io::Error::other)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "cargo_validation_cache_tests.rs"]
mod tests;
