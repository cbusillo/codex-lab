use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use tracing::warn;

static NEXT_TRANSACTION_ID: AtomicU64 = AtomicU64::new(/*v*/ 0);

#[cfg(any(test, windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RepairOutcome {
    Empty,
    Current,
    Unchanged,
    Committed,
}

#[cfg(any(test, windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TransactionKind {
    FirstPublish,
    ReplaceExisting,
}

#[cfg(any(test, windows))]
impl TransactionKind {
    fn label(self) -> &'static str {
        match self {
            Self::FirstPublish => "first",
            Self::ReplaceExisting => "replace",
        }
    }
}

#[cfg(any(test, windows))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TransactionPaths {
    pub(super) marker: PathBuf,
    pub(super) temp: PathBuf,
    pub(super) backup: PathBuf,
    kind: TransactionKind,
}

#[cfg(any(test, windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionState {
    Empty,
    Destination,
    Temp,
    Backup,
    DestinationTemp,
    DestinationBackup,
    TempBackup,
    All,
}

pub(crate) fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    #[cfg(windows)]
    {
        write_file_atomically_windows(path, contents)
    }

    #[cfg(not(windows))]
    {
        write_file_atomically_unix(path, contents)
    }
}

#[cfg(any(test, windows))]
pub(crate) fn readable_path(path: &Path) -> Result<Option<PathBuf>> {
    if path.try_exists()? {
        return Ok(Some(path.to_path_buf()));
    }
    let Some(transaction) = find_transaction(path)? else {
        return if path.try_exists()? {
            Ok(Some(path.to_path_buf()))
        } else {
            Ok(None)
        };
    };
    let state = transaction_state(path, &transaction)?;
    match (transaction.kind, state) {
        (TransactionKind::FirstPublish, TransactionState::Destination) => {
            Ok(Some(path.to_path_buf()))
        }
        (TransactionKind::FirstPublish, TransactionState::Temp) => Ok(Some(transaction.temp)),
        (
            TransactionKind::FirstPublish,
            TransactionState::Empty
            | TransactionState::Backup
            | TransactionState::DestinationTemp
            | TransactionState::DestinationBackup
            | TransactionState::TempBackup
            | TransactionState::All,
        ) => indeterminate_transaction(path, &transaction, state),
        (
            TransactionKind::ReplaceExisting,
            TransactionState::Destination
            | TransactionState::DestinationTemp
            | TransactionState::DestinationBackup,
        ) => Ok(Some(path.to_path_buf())),
        (
            TransactionKind::ReplaceExisting,
            TransactionState::Backup | TransactionState::TempBackup,
        ) => Ok(Some(transaction.backup)),
        (
            TransactionKind::ReplaceExisting,
            TransactionState::Empty | TransactionState::Temp | TransactionState::All,
        ) => indeterminate_transaction(path, &transaction, state),
    }
}

#[cfg(any(test, windows))]
pub(crate) fn recovery_artifacts_exist(path: &Path) -> Result<bool> {
    Ok(find_transaction(path)?.is_some())
}

#[cfg(any(test, windows))]
pub(crate) fn recover_interrupted_write(path: &Path) -> Result<RepairOutcome> {
    let Some(transaction) = find_transaction(path)? else {
        return if path.try_exists()? {
            Ok(RepairOutcome::Current)
        } else {
            Ok(RepairOutcome::Empty)
        };
    };
    let state = transaction_state(path, &transaction)?;
    match (transaction.kind, state) {
        (TransactionKind::FirstPublish, TransactionState::Destination) => {
            sync_file(path)?;
            remove_owned_file(&transaction.marker)?;
            Ok(RepairOutcome::Committed)
        }
        (TransactionKind::FirstPublish, TransactionState::Temp) => {
            move_file(&transaction.temp, path).with_context(|| {
                format!(
                    "failed to publish staged secrets file {} to {}",
                    transaction.temp.display(),
                    path.display()
                )
            })?;
            sync_file(path)?;
            remove_owned_file(&transaction.marker)?;
            Ok(RepairOutcome::Committed)
        }
        (
            TransactionKind::FirstPublish,
            TransactionState::Empty
            | TransactionState::Backup
            | TransactionState::DestinationTemp
            | TransactionState::DestinationBackup
            | TransactionState::TempBackup
            | TransactionState::All,
        ) => indeterminate_transaction(path, &transaction, state),
        (TransactionKind::ReplaceExisting, TransactionState::Destination) => {
            sync_file(path)?;
            remove_owned_file(&transaction.marker)?;
            Ok(RepairOutcome::Current)
        }
        (TransactionKind::ReplaceExisting, TransactionState::DestinationTemp) => {
            remove_owned_file(&transaction.temp)?;
            remove_owned_file(&transaction.marker)?;
            Ok(RepairOutcome::Unchanged)
        }
        (TransactionKind::ReplaceExisting, TransactionState::DestinationBackup) => {
            sync_file(path)?;
            remove_owned_file(&transaction.backup)?;
            remove_owned_file(&transaction.marker)?;
            Ok(RepairOutcome::Committed)
        }
        (TransactionKind::ReplaceExisting, TransactionState::Backup) => {
            move_file(&transaction.backup, path).with_context(|| {
                format!(
                    "failed to restore secrets backup {} to {}",
                    transaction.backup.display(),
                    path.display()
                )
            })?;
            sync_file(path)?;
            remove_owned_file(&transaction.marker)?;
            Ok(RepairOutcome::Unchanged)
        }
        (TransactionKind::ReplaceExisting, TransactionState::TempBackup) => {
            move_file(&transaction.backup, path).with_context(|| {
                format!(
                    "failed to restore secrets backup {} to {}",
                    transaction.backup.display(),
                    path.display()
                )
            })?;
            sync_file(path)?;
            remove_owned_file(&transaction.temp)?;
            remove_owned_file(&transaction.marker)?;
            Ok(RepairOutcome::Unchanged)
        }
        (
            TransactionKind::ReplaceExisting,
            TransactionState::Empty | TransactionState::Temp | TransactionState::All,
        ) => indeterminate_transaction(path, &transaction, state),
    }
}

#[cfg(any(test, windows))]
fn recover_failed_replace(path: &Path) -> Result<()> {
    let transaction = find_transaction(path)?.with_context(|| {
        format!(
            "ReplaceFileW failed without a transaction marker for {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        transaction.kind == TransactionKind::ReplaceExisting,
        "ReplaceFileW failed with a first-publish marker for {}",
        path.display()
    );
    let state = transaction_state(path, &transaction)?;
    match state {
        TransactionState::DestinationTemp => {
            remove_owned_file(&transaction.temp)?;
            remove_owned_file(&transaction.marker)
        }
        TransactionState::TempBackup => {
            move_file(&transaction.backup, path).with_context(|| {
                format!(
                    "failed to restore secrets backup {} to {}",
                    transaction.backup.display(),
                    path.display()
                )
            })?;
            sync_file(path)?;
            remove_owned_file(&transaction.temp)?;
            remove_owned_file(&transaction.marker)
        }
        TransactionState::Empty
        | TransactionState::Destination
        | TransactionState::Temp
        | TransactionState::Backup
        | TransactionState::DestinationBackup
        | TransactionState::All => indeterminate_transaction(path, &transaction, state),
    }
}

fn write_temp_file(path: &Path, contents: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(/*mode*/ 0o600);
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create temp file at {}", path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("failed to write temp file at {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync temp file at {}", path.display()))
}

#[cfg(not(windows))]
fn write_file_atomically_unix(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().with_context(|| {
        format!(
            "failed to compute parent directory for secrets file at {}",
            path.display()
        )
    })?;
    let temp = random_temp_path(path)?;
    if let Err(error) = write_temp_file(&temp, contents) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error).with_context(|| {
            format!(
                "failed to atomically replace secrets file at {} with {}",
                path.display(),
                temp.display()
            )
        });
    }
    #[cfg(unix)]
    if let Err(error) = sync_parent_directory(dir) {
        warn!("failed to sync secrets directory after atomic replace: {error:#}");
    }
    Ok(())
}

#[cfg(not(windows))]
fn random_temp_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().with_context(|| {
        format!(
            "failed to compute parent directory for secrets file at {}",
            path.display()
        )
    })?;
    let filename = path.file_name().with_context(|| {
        format!(
            "failed to compute filename for secrets file at {}",
            path.display()
        )
    })?;
    let filename = filename.to_string_lossy();
    let id = next_transaction_id();
    Ok(parent.join(format!(".{filename}.tmp-{id}")))
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("failed to open secrets dir {} for sync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync secrets dir {}", path.display()))
}

#[cfg(windows)]
fn write_file_atomically_windows(path: &Path, contents: &[u8]) -> Result<()> {
    recover_interrupted_write(path)?;
    anyhow::ensure!(
        find_transaction(path)?.is_none(),
        "could not clear the active secrets transaction for {}",
        path.display()
    );
    let kind = if path.try_exists()? {
        TransactionKind::ReplaceExisting
    } else {
        TransactionKind::FirstPublish
    };
    let transaction = create_transaction(path, contents, kind)?;

    match kind {
        TransactionKind::FirstPublish => match move_file(&transaction.temp, path) {
            Ok(()) => {
                if let Err(error) = recover_interrupted_write(path) {
                    warn!(
                        "secrets file was published at {} but transaction cleanup failed: {error:#}",
                        path.display()
                    );
                }
                Ok(())
            }
            Err(error) => match recover_interrupted_write(path) {
                Ok(RepairOutcome::Committed) => Ok(()),
                Ok(RepairOutcome::Empty | RepairOutcome::Current | RepairOutcome::Unchanged) => {
                    Err(error).with_context(|| {
                        format!("failed to publish first secrets file at {}", path.display())
                    })
                }
                Err(recovery_error) => anyhow::bail!(
                    "{error}; first-publish recovery for {} also failed: {recovery_error:#}",
                    path.display()
                ),
            },
        },
        TransactionKind::ReplaceExisting => {
            match replace_file_windows(path, &transaction.temp, &transaction.backup) {
                Ok(()) => {
                    if let Err(error) = recover_interrupted_write(path) {
                        warn!(
                            "secrets file replacement committed at {} but transaction cleanup failed: {error:#}",
                            path.display()
                        );
                    }
                    Ok(())
                }
                Err(error) => match recover_failed_replace(path) {
                    Ok(()) => Err(error).with_context(|| {
                        format!("failed to replace secrets file at {}", path.display())
                    }),
                    Err(recovery_error) => anyhow::bail!(
                        "{error}; replacement recovery for {} also failed: {recovery_error:#}",
                        path.display()
                    ),
                },
            }
        }
    }
}

#[cfg(windows)]
fn create_transaction(
    path: &Path,
    contents: &[u8],
    kind: TransactionKind,
) -> Result<TransactionPaths> {
    let transaction = transaction_paths(path, kind, &next_transaction_id())?;
    anyhow::ensure!(
        !transaction.temp.try_exists()?
            && !transaction.backup.try_exists()?
            && !transaction.marker.try_exists()?,
        "secrets transaction paths already exist for {}",
        path.display()
    );
    if let Err(error) = write_temp_file(&transaction.temp, contents) {
        let _ = fs::remove_file(&transaction.temp);
        return Err(error);
    }

    let marker = match fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&transaction.marker)
    {
        Ok(marker) => marker,
        Err(error) => {
            let _ = fs::remove_file(&transaction.temp);
            return Err(error).with_context(|| {
                format!(
                    "failed to create secrets transaction marker {}",
                    transaction.marker.display()
                )
            });
        }
    };
    if let Err(error) = marker.sync_all() {
        drop(marker);
        if let Err(cleanup_error) = fs::remove_file(&transaction.marker) {
            anyhow::bail!(
                "failed to sync secrets transaction marker {}: {error}; preserving {} because marker cleanup also failed: {cleanup_error}",
                transaction.marker.display(),
                transaction.temp.display()
            );
        }
        let _ = fs::remove_file(&transaction.temp);
        return Err(error).with_context(|| {
            format!(
                "failed to sync secrets transaction marker {}",
                transaction.marker.display()
            )
        });
    }
    Ok(transaction)
}

#[cfg(any(test, windows))]
pub(super) fn transaction_paths(
    path: &Path,
    kind: TransactionKind,
    id: &str,
) -> Result<TransactionPaths> {
    anyhow::ensure!(valid_transaction_id(id), "invalid transaction id {id:?}");
    let parent = path.parent().with_context(|| {
        format!(
            "failed to compute parent directory for secrets file at {}",
            path.display()
        )
    })?;
    let filename = path
        .file_name()
        .and_then(|filename| filename.to_str())
        .with_context(|| {
            format!(
                "secrets filename at {} must be valid Unicode",
                path.display()
            )
        })?;
    let base = format!(".{filename}.txn-{}-{id}", kind.label());
    Ok(TransactionPaths {
        marker: parent.join(format!("{base}.ready")),
        temp: parent.join(format!("{base}.tmp")),
        backup: parent.join(format!("{base}.bak")),
        kind,
    })
}

#[cfg(any(test, windows))]
fn find_transaction(path: &Path) -> Result<Option<TransactionPaths>> {
    let parent = path.parent().with_context(|| {
        format!(
            "failed to compute parent directory for secrets file at {}",
            path.display()
        )
    })?;
    let filename = path
        .file_name()
        .and_then(|filename| filename.to_str())
        .with_context(|| {
            format!(
                "secrets filename at {} must be valid Unicode",
                path.display()
            )
        })?;
    let prefix = format!(".{filename}.txn-");
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect secrets transactions in {}",
                    parent.display()
                )
            });
        }
    };
    let mut found: Option<TransactionPaths> = None;
    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect a secrets transaction in {}",
                parent.display()
            )
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(body) = name
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(".ready"))
        else {
            continue;
        };
        let (kind, id) = if let Some(id) = body.strip_prefix("first-") {
            (TransactionKind::FirstPublish, id)
        } else if let Some(id) = body.strip_prefix("replace-") {
            (TransactionKind::ReplaceExisting, id)
        } else {
            anyhow::bail!(
                "invalid secrets transaction marker {}; preserving it",
                entry.path().display()
            );
        };
        anyhow::ensure!(
            valid_transaction_id(id),
            "invalid secrets transaction marker {}; preserving it",
            entry.path().display()
        );
        anyhow::ensure!(
            entry.file_type()?.is_file(),
            "secrets transaction marker {} is not a file; preserving it",
            entry.path().display()
        );
        anyhow::ensure!(
            entry.metadata()?.len() == 0,
            "secrets transaction marker {} is not empty; preserving it",
            entry.path().display()
        );
        let transaction = transaction_paths(path, kind, id)?;
        if let Some(existing) = &found {
            anyhow::bail!(
                "multiple secrets transactions exist for {}: {} and {}; preserving both",
                path.display(),
                existing.marker.display(),
                transaction.marker.display()
            );
        }
        found = Some(transaction);
    }
    Ok(found)
}

#[cfg(any(test, windows))]
fn transaction_state(path: &Path, transaction: &TransactionPaths) -> Result<TransactionState> {
    let destination = path.try_exists()?;
    let temp = transaction.temp.try_exists()?;
    let backup = transaction.backup.try_exists()?;
    Ok(match (destination, temp, backup) {
        (false, false, false) => TransactionState::Empty,
        (true, false, false) => TransactionState::Destination,
        (false, true, false) => TransactionState::Temp,
        (false, false, true) => TransactionState::Backup,
        (true, true, false) => TransactionState::DestinationTemp,
        (true, false, true) => TransactionState::DestinationBackup,
        (false, true, true) => TransactionState::TempBackup,
        (true, true, true) => TransactionState::All,
    })
}

#[cfg(any(test, windows))]
fn indeterminate_transaction<T>(
    path: &Path,
    transaction: &TransactionPaths,
    state: TransactionState,
) -> Result<T> {
    anyhow::bail!(
        "indeterminate {state:?} secrets transaction at {}; preserving {}, {}, {}, and {}",
        path.display(),
        path.display(),
        transaction.temp.display(),
        transaction.backup.display(),
        transaction.marker.display()
    )
}

#[cfg(any(test, windows))]
fn valid_transaction_id(id: &str) -> bool {
    if id.len() > 96 {
        return false;
    }
    let mut parts = id.split('-');
    let valid_part =
        |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_hexdigit());
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(process), Some(nanos), Some(sequence), None)
            if valid_part(process) && valid_part(nanos) && valid_part(sequence)
    )
}

#[cfg(any(test, windows))]
fn remove_owned_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove transaction file {}", path.display())),
    }
}

#[cfg(any(test, windows))]
fn move_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        move_file_windows(source, destination)
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

fn next_transaction_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(/*default*/ 0, |duration| duration.as_nanos());
    let sequence = NEXT_TRANSACTION_ID.fetch_add(/*val*/ 1, Ordering::Relaxed);
    let process = std::process::id();
    format!("{process:x}-{nanos:x}-{sequence:x}")
}

#[cfg(windows)]
fn replace_file_windows(destination: &Path, temp: &Path, backup: &Path) -> std::io::Result<()> {
    use std::ffi::c_void;

    unsafe extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut c_void,
            reserved: *mut c_void,
        ) -> i32;
    }

    let destination = std::path::absolute(destination)?;
    let parent = destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("replacement path has no parent: {}", destination.display()),
        )
    })?;
    let temp = parent.join(temp.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "temp has no filename")
    })?);
    let backup = parent.join(backup.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "backup has no filename")
    })?);
    let destination = wide_absolute_path(&destination)?;
    let temp = wide_absolute_path(&temp)?;
    let backup = wide_absolute_path(&backup)?;
    let replaced = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            temp.as_ptr(),
            backup.as_ptr(),
            /*replace_flags*/ 0,
            /*exclude*/ std::ptr::null_mut(),
            /*reserved*/ std::ptr::null_mut(),
        )
    };
    if replaced == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn move_file_windows(source: &Path, destination: &Path) -> std::io::Result<()> {
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    let destination = std::path::absolute(destination)?;
    let parent = destination.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("move destination has no parent: {}", destination.display()),
        )
    })?;
    let source = parent.join(source.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no filename")
    })?);
    let source = wide_absolute_path(&source)?;
    let destination = wide_absolute_path(&destination)?;
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            /*flags*/ MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn wide_absolute_path(path: &Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;

    const SEPARATOR: u16 = b'\\' as u16;
    const VERBATIM_PREFIX: [u16; 4] = [SEPARATOR, SEPARATOR, b'?' as u16, SEPARATOR];
    const UNC_PREFIX: [u16; 8] = [
        SEPARATOR,
        SEPARATOR,
        b'?' as u16,
        SEPARATOR,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        SEPARATOR,
    ];

    let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows paths cannot contain NUL",
        ));
    }
    let mut result = if wide.starts_with(&VERBATIM_PREFIX) {
        wide
    } else if wide.starts_with(&[SEPARATOR, SEPARATOR]) {
        UNC_PREFIX
            .into_iter()
            .chain(wide.into_iter().skip(2))
            .collect()
    } else {
        VERBATIM_PREFIX.into_iter().chain(wide).collect()
    };
    result.push(/*value*/ 0);
    Ok(result)
}

#[cfg(test)]
#[path = "atomic_file_tests.rs"]
mod tests;
