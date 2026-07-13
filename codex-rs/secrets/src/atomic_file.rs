use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
#[cfg(unix)]
use tracing::warn;

#[cfg(any(test, windows))]
#[path = "atomic_file/marker.rs"]
mod marker;
#[cfg(any(test, windows))]
#[path = "atomic_file/transaction.rs"]
mod transaction;
#[cfg(windows)]
#[path = "atomic_file/windows.rs"]
mod windows;

#[cfg(test)]
pub(super) use marker::MarkerRecord;
#[cfg(any(test, windows))]
pub(super) use transaction::RepairOutcome;
#[cfg(any(test, windows))]
pub(super) use transaction::TransactionKind;
#[cfg(any(test, windows))]
pub(super) use transaction::TransactionPaths;
#[cfg(any(test, windows))]
pub(crate) use transaction::readable_path;
#[cfg(any(test, windows))]
use transaction::recover_failed_replace;
#[cfg(any(test, windows))]
pub(crate) use transaction::recover_interrupted_write;
#[cfg(any(test, windows))]
pub(super) use transaction::transaction_paths;
#[cfg(windows)]
pub(super) use transaction::write_transaction_marker;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(/*v*/ 0);

#[cfg(windows)]
pub(crate) fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    windows::write_file_atomically(path, contents)
}

#[cfg(not(windows))]
pub(crate) fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().with_context(|| {
        format!(
            "failed to compute parent directory for secrets file at {}",
            path.display()
        )
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(/*default*/ 0, |duration| duration.as_nanos());
    let filename = path.file_name().with_context(|| {
        format!(
            "failed to compute filename for secrets file at {}",
            path.display()
        )
    })?;
    let sequence = NEXT_TEMP_ID.fetch_add(/*val*/ 1, Ordering::Relaxed);
    let temp = dir.join(format!(
        ".{}.tmp-{}-{nonce}-{sequence}",
        filename.to_string_lossy(),
        std::process::id()
    ));

    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(/*mode*/ 0o600);
    let mut file = options
        .open(&temp)
        .with_context(|| format!("failed to create temp secrets file at {}", temp.display()))?;
    file.write_all(contents)
        .with_context(|| format!("failed to write temp secrets file at {}", temp.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync temp secrets file at {}", temp.display()))?;
    drop(file);

    match fs::rename(&temp, path) {
        Ok(()) => {
            #[cfg(unix)]
            if let Err(error) = fs::File::open(dir).and_then(|directory| directory.sync_all()) {
                warn!("failed to sync secrets directory after atomic replace: {error}");
            }
            Ok(())
        }
        Err(initial_error) => {
            #[cfg(windows)]
            if path.exists() {
                fs::remove_file(path).with_context(|| {
                    format!(
                        "failed to remove existing secrets file at {} before replace",
                        path.display()
                    )
                })?;
                fs::rename(&temp, path).with_context(|| {
                    format!(
                        "failed to replace secrets file at {} with {}",
                        path.display(),
                        temp.display()
                    )
                })?;
                return Ok(());
            }

            let _ = fs::remove_file(&temp);
            Err(initial_error).with_context(|| {
                format!(
                    "failed to atomically replace secrets file at {} with {}",
                    path.display(),
                    temp.display()
                )
            })
        }
    }
}

#[cfg(windows)]
fn write_new_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(/*mode*/ 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create temp file at {}", path.display()))?;
    let write_result = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("failed to write and sync temp file at {}", path.display()));
    drop(file);
    if let Err(error) = write_result {
        if let Err(cleanup_error) = fs::remove_file(path)
            && cleanup_error.kind() != std::io::ErrorKind::NotFound
        {
            anyhow::bail!(
                "{error:#}; failed to remove incomplete temp file {}: {cleanup_error}",
                path.display()
            );
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(any(test, windows))]
fn move_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        windows::move_file(source, destination)
    }

    #[cfg(not(windows))]
    {
        fs::rename(source, destination)
    }
}

#[cfg(any(test, windows))]
fn sync_file(path: &Path) -> Result<()> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open committed secrets file {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync committed secrets file {}", path.display()))
}

#[cfg(windows)]
fn next_transaction_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(/*default*/ 0, |duration| duration.as_nanos());
    let sequence = NEXT_TEMP_ID.fetch_add(/*val*/ 1, Ordering::Relaxed);
    let process = std::process::id();
    format!("{process:x}-{nanos:x}-{sequence:x}")
}

#[cfg(test)]
#[path = "atomic_file_tests.rs"]
mod tests;
