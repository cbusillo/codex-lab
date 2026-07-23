use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;

use super::marker::MarkerRecord;
use super::marker::fingerprint_file;
use super::move_file;
use super::sync_file;
#[cfg(windows)]
use super::write_new_file;

const ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: i32 = 1177;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepairOutcome {
    Empty,
    Current,
    Unchanged,
    Committed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransactionKind {
    FirstPublish,
    ReplaceExisting,
}

impl TransactionKind {
    fn label(self) -> &'static str {
        match self {
            Self::FirstPublish => "first",
            Self::ReplaceExisting => "replace",
        }
    }

    pub(super) fn marker_byte(self) -> u8 {
        match self {
            Self::FirstPublish => 1,
            Self::ReplaceExisting => 2,
        }
    }

    pub(super) fn from_marker_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::FirstPublish),
            2 => Some(Self::ReplaceExisting),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransactionPaths {
    pub(crate) marker: PathBuf,
    pub(crate) temp: PathBuf,
    pub(crate) backup: PathBuf,
    pub(crate) kind: TransactionKind,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadableGeneration {
    Destination,
    Temp,
    Backup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryAction {
    RemoveMarker,
    SyncDestination,
    PublishTemp,
    DiscardTemp,
    DiscardBackup,
    RestoreBackupAndDiscardTemp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedState {
    readable: ReadableGeneration,
    action: RecoveryAction,
    outcome: RepairOutcome,
}

#[derive(Debug)]
struct ActiveTransaction {
    paths: TransactionPaths,
    marker: MarkerRecord,
}

pub(crate) fn readable_path(path: &Path) -> Result<Option<PathBuf>> {
    let Some(transaction) = find_transaction(path)? else {
        return if path.try_exists()? {
            Ok(Some(path.to_path_buf()))
        } else {
            Ok(None)
        };
    };
    let state = transaction_state(path, &transaction.paths)?;
    let validated = validate_transaction_state(path, &transaction, state)?;
    Ok(Some(match validated.readable {
        ReadableGeneration::Destination => path.to_path_buf(),
        ReadableGeneration::Temp => transaction.paths.temp,
        ReadableGeneration::Backup => transaction.paths.backup,
    }))
}

pub(crate) fn recover_interrupted_write(path: &Path) -> Result<RepairOutcome> {
    let Some(transaction) = find_transaction(path)? else {
        return if path.try_exists()? {
            Ok(RepairOutcome::Current)
        } else {
            Ok(RepairOutcome::Empty)
        };
    };
    let state = transaction_state(path, &transaction.paths)?;
    let validated = validate_transaction_state(path, &transaction, state)?;
    execute_recovery(path, &transaction.paths, validated)
}

pub(super) fn recover_failed_replace(path: &Path, replace_error: &std::io::Error) -> Result<()> {
    let transaction = find_transaction(path)?.with_context(|| {
        format!(
            "ReplaceFileW failed without a transaction marker for {}",
            path.display()
        )
    })?;
    anyhow::ensure!(
        transaction.paths.kind == TransactionKind::ReplaceExisting,
        "ReplaceFileW failed with a first-publish marker for {}",
        path.display()
    );
    let state = transaction_state(path, &transaction.paths)?;
    let validated = validate_transaction_state(path, &transaction, state)?;
    let documented_failure = match validated.action {
        RecoveryAction::DiscardTemp => true,
        RecoveryAction::RestoreBackupAndDiscardTemp => {
            replace_error.raw_os_error() == Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT_2)
        }
        RecoveryAction::RemoveMarker
        | RecoveryAction::SyncDestination
        | RecoveryAction::PublishTemp
        | RecoveryAction::DiscardBackup => false,
    };
    if !documented_failure {
        anyhow::bail!(
            "ReplaceFileW reported failure in undocumented {state:?} state at {}; preserving {}, {}, {}, and {}",
            path.display(),
            path.display(),
            transaction.paths.temp.display(),
            transaction.paths.backup.display(),
            transaction.paths.marker.display()
        );
    }
    execute_recovery(path, &transaction.paths, validated)?;
    Ok(())
}

#[cfg(windows)]
pub(crate) fn write_transaction_marker(
    transaction: &TransactionPaths,
    source: Option<&[u8]>,
    replacement: &[u8],
) -> Result<()> {
    let marker = MarkerRecord::new(transaction.kind, source, replacement)?;
    write_new_file(&transaction.marker, &marker.encode(transaction.kind)).with_context(|| {
        format!(
            "failed to write secrets transaction marker {}",
            transaction.marker.display()
        )
    })
}

pub(crate) fn transaction_paths(
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

fn find_transaction(path: &Path) -> Result<Option<ActiveTransaction>> {
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
    let mut found: Option<ActiveTransaction> = None;
    for entry in entries {
        let entry = entry?;
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
            valid_transaction_id(id) && entry.file_type()?.is_file(),
            "invalid secrets transaction marker {}; preserving it",
            entry.path().display()
        );
        let paths = transaction_paths(path, kind, id)?;
        let marker = MarkerRecord::decode(&fs::read(entry.path())?, kind, &entry.path())?;
        if let Some(existing) = &found {
            anyhow::bail!(
                "multiple secrets transactions exist for {}: {} and {}; preserving both",
                path.display(),
                existing.paths.marker.display(),
                paths.marker.display()
            );
        }
        found = Some(ActiveTransaction { paths, marker });
    }
    Ok(found)
}

fn transaction_state(path: &Path, transaction: &TransactionPaths) -> Result<TransactionState> {
    Ok(
        match (
            path.try_exists()?,
            transaction.temp.try_exists()?,
            transaction.backup.try_exists()?,
        ) {
            (false, false, false) => TransactionState::Empty,
            (true, false, false) => TransactionState::Destination,
            (false, true, false) => TransactionState::Temp,
            (false, false, true) => TransactionState::Backup,
            (true, true, false) => TransactionState::DestinationTemp,
            (true, false, true) => TransactionState::DestinationBackup,
            (false, true, true) => TransactionState::TempBackup,
            (true, true, true) => TransactionState::All,
        },
    )
}

fn validate_transaction_state(
    path: &Path,
    transaction: &ActiveTransaction,
    state: TransactionState,
) -> Result<ValidatedState> {
    let paths = &transaction.paths;
    let marker = transaction.marker;
    let validated = match (paths.kind, state) {
        (TransactionKind::FirstPublish, TransactionState::Destination) => {
            ensure_generation(path, marker.replacement, paths)?;
            ValidatedState {
                readable: ReadableGeneration::Destination,
                action: RecoveryAction::SyncDestination,
                outcome: RepairOutcome::Committed,
            }
        }
        (TransactionKind::FirstPublish, TransactionState::Temp) => {
            ensure_generation(&paths.temp, marker.replacement, paths)?;
            ValidatedState {
                readable: ReadableGeneration::Temp,
                action: RecoveryAction::PublishTemp,
                outcome: RepairOutcome::Committed,
            }
        }
        (TransactionKind::ReplaceExisting, TransactionState::Destination) => {
            let actual = fingerprint_file(path)?;
            if actual == marker.replacement {
                ValidatedState {
                    readable: ReadableGeneration::Destination,
                    action: RecoveryAction::SyncDestination,
                    outcome: RepairOutcome::Committed,
                }
            } else if actual == marker.source {
                ValidatedState {
                    readable: ReadableGeneration::Destination,
                    action: RecoveryAction::RemoveMarker,
                    outcome: RepairOutcome::Unchanged,
                }
            } else {
                return generation_mismatch(path, paths);
            }
        }
        (TransactionKind::ReplaceExisting, TransactionState::DestinationTemp) => {
            ensure_generation(path, marker.source, paths)?;
            ensure_generation(&paths.temp, marker.replacement, paths)?;
            ValidatedState {
                readable: ReadableGeneration::Destination,
                action: RecoveryAction::DiscardTemp,
                outcome: RepairOutcome::Unchanged,
            }
        }
        (TransactionKind::ReplaceExisting, TransactionState::DestinationBackup) => {
            ensure_generation(path, marker.replacement, paths)?;
            ensure_generation(&paths.backup, marker.source, paths)?;
            ValidatedState {
                readable: ReadableGeneration::Destination,
                action: RecoveryAction::DiscardBackup,
                outcome: RepairOutcome::Committed,
            }
        }
        (TransactionKind::ReplaceExisting, TransactionState::TempBackup) => {
            ensure_generation(&paths.temp, marker.replacement, paths)?;
            ensure_generation(&paths.backup, marker.source, paths)?;
            ValidatedState {
                readable: ReadableGeneration::Backup,
                action: RecoveryAction::RestoreBackupAndDiscardTemp,
                outcome: RepairOutcome::Unchanged,
            }
        }
        (
            TransactionKind::FirstPublish,
            TransactionState::Empty
            | TransactionState::Backup
            | TransactionState::DestinationTemp
            | TransactionState::DestinationBackup
            | TransactionState::TempBackup
            | TransactionState::All,
        )
        | (
            TransactionKind::ReplaceExisting,
            TransactionState::Empty
            | TransactionState::Temp
            | TransactionState::Backup
            | TransactionState::All,
        ) => return indeterminate_transaction(path, paths, state),
    };
    Ok(validated)
}

fn execute_recovery(
    path: &Path,
    transaction: &TransactionPaths,
    validated: ValidatedState,
) -> Result<RepairOutcome> {
    match validated.action {
        RecoveryAction::RemoveMarker => {}
        RecoveryAction::SyncDestination => sync_file(path)?,
        RecoveryAction::PublishTemp => {
            move_generation(&transaction.temp, path, "publish staged secrets file")?;
            sync_file(path)?;
        }
        RecoveryAction::DiscardTemp => remove_owned_file(&transaction.temp)?,
        RecoveryAction::DiscardBackup => {
            sync_file(path)?;
            remove_owned_file(&transaction.backup)?;
        }
        RecoveryAction::RestoreBackupAndDiscardTemp => {
            move_generation(&transaction.backup, path, "restore secrets backup")?;
            sync_file(path)?;
            remove_owned_file(&transaction.temp)?;
        }
    }
    remove_owned_file(&transaction.marker)?;
    Ok(validated.outcome)
}

fn move_generation(source: &Path, destination: &Path, action: &str) -> Result<()> {
    move_file(source, destination).with_context(|| {
        format!(
            "failed to {action} {} to {}",
            source.display(),
            destination.display()
        )
    })
}

fn ensure_generation(
    path: &Path,
    expected: [u8; 32],
    transaction: &TransactionPaths,
) -> Result<()> {
    if fingerprint_file(path)? != expected {
        return generation_mismatch(path, transaction);
    }
    Ok(())
}

fn generation_mismatch<T>(path: &Path, transaction: &TransactionPaths) -> Result<T> {
    anyhow::bail!(
        "secrets generation at {} does not match transaction marker {}; preserving all transaction files",
        path.display(),
        transaction.marker.display()
    )
}

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

fn remove_owned_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove transaction file {}", path.display())),
    }
}
