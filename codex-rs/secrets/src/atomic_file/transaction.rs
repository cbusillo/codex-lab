use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;

use super::marker::MarkerRecord;
use super::marker::fingerprint_file;

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

    #[cfg(test)]
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
    kind: TransactionKind,
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
    Ok(Some(match validate_state(path, &transaction, state)? {
        ReadableGeneration::Destination => path.to_path_buf(),
        ReadableGeneration::Temp => transaction.paths.temp,
        ReadableGeneration::Backup => transaction.paths.backup,
    }))
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

fn validate_state(
    path: &Path,
    transaction: &ActiveTransaction,
    state: TransactionState,
) -> Result<ReadableGeneration> {
    let paths = &transaction.paths;
    let marker = transaction.marker;
    match (paths.kind, state) {
        (TransactionKind::FirstPublish, TransactionState::Destination) => {
            ensure_generation(path, marker.replacement, paths)?;
            Ok(ReadableGeneration::Destination)
        }
        (TransactionKind::FirstPublish, TransactionState::Temp) => {
            ensure_generation(&paths.temp, marker.replacement, paths)?;
            Ok(ReadableGeneration::Temp)
        }
        (TransactionKind::ReplaceExisting, TransactionState::Destination) => {
            let actual = fingerprint_file(path)?;
            if actual == marker.source || actual == marker.replacement {
                Ok(ReadableGeneration::Destination)
            } else {
                generation_mismatch(path, paths)
            }
        }
        (TransactionKind::ReplaceExisting, TransactionState::DestinationTemp) => {
            ensure_generation(path, marker.source, paths)?;
            ensure_generation(&paths.temp, marker.replacement, paths)?;
            Ok(ReadableGeneration::Destination)
        }
        (TransactionKind::ReplaceExisting, TransactionState::DestinationBackup) => {
            ensure_generation(path, marker.replacement, paths)?;
            ensure_generation(&paths.backup, marker.source, paths)?;
            Ok(ReadableGeneration::Destination)
        }
        (TransactionKind::ReplaceExisting, TransactionState::TempBackup) => {
            ensure_generation(&paths.temp, marker.replacement, paths)?;
            ensure_generation(&paths.backup, marker.source, paths)?;
            Ok(ReadableGeneration::Backup)
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
        ) => indeterminate_transaction(path, paths, state),
    }
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
