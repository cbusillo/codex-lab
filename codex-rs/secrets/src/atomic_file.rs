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

#[cfg(test)]
pub(super) use marker::MarkerRecord;
#[cfg(any(test, windows))]
pub(super) use transaction::TransactionKind;
#[cfg(test)]
pub(super) use transaction::TransactionPaths;
#[cfg(any(test, windows))]
pub(crate) use transaction::readable_path;
#[cfg(test)]
pub(super) use transaction::transaction_paths;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(/*v*/ 0);

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

#[cfg(test)]
#[path = "atomic_file_tests.rs"]
mod tests;
