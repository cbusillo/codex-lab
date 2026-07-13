use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;

use super::RepairOutcome;
use super::TransactionKind;
use super::TransactionPaths;
use super::readable_path;
use super::recover_failed_replace;
use super::recover_interrupted_write;
use super::recovery_artifacts_exist;
use super::transaction_paths;
use super::write_file_atomically;
use super::write_transaction_marker;

const OLD: &[u8] = b"old";
const NEW: &[u8] = b"new";
const ZERO_MARKER: [u8; 74] = [0; 74];

type Bytes = Option<&'static [u8]>;

#[derive(Clone, Copy)]
enum ExpectedRead {
    Destination,
    Temp,
    Backup,
    Error,
}

#[derive(Clone, Copy)]
enum ExpectedRecovery {
    Success(RepairOutcome, Bytes),
    Error,
}

#[derive(Clone, Copy)]
struct Case(
    &'static str,
    TransactionKind,
    Bytes,
    Bytes,
    Bytes,
    ExpectedRead,
    ExpectedRecovery,
);

#[derive(Debug, PartialEq, Eq)]
struct Files {
    destination: Option<Vec<u8>>,
    temp: Option<Vec<u8>>,
    backup: Option<Vec<u8>>,
    marker: Option<Vec<u8>>,
}

fn read_if_present(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    path.try_exists()?
        .then(|| fs::read(path))
        .transpose()
        .map_err(Into::into)
}

fn files(destination: &Path, transaction: &TransactionPaths) -> anyhow::Result<Files> {
    Ok(Files {
        destination: read_if_present(destination)?,
        temp: read_if_present(&transaction.temp)?,
        backup: read_if_present(&transaction.backup)?,
        marker: read_if_present(&transaction.marker)?,
    })
}

fn stage(
    destination: &Path,
    kind: TransactionKind,
    destination_bytes: Bytes,
    temp: Bytes,
    backup: Bytes,
) -> anyhow::Result<TransactionPaths> {
    let transaction = transaction_paths(destination, kind, "a1-b2-c3")?;
    for (path, contents) in [
        (destination, destination_bytes),
        (&transaction.temp, temp),
        (&transaction.backup, backup),
    ] {
        if let Some(contents) = contents {
            fs::write(path, contents)?;
        }
    }
    let source = (kind == TransactionKind::ReplaceExisting).then_some(OLD);
    write_transaction_marker(&transaction, source, NEW)?;
    Ok(transaction)
}

fn run_case(case: Case) -> anyhow::Result<()> {
    let Case(name, kind, destination_bytes, temp, backup, expected_read, recovery) = case;
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = stage(&destination, kind, destination_bytes, temp, backup)?;
    let before = files(&destination, &transaction)?;

    match expected_read {
        ExpectedRead::Destination => {
            assert_eq!(readable_path(&destination)?, Some(destination.clone()))
        }
        ExpectedRead::Temp => {
            assert_eq!(readable_path(&destination)?, Some(transaction.temp.clone()))
        }
        ExpectedRead::Backup => {
            assert_eq!(
                readable_path(&destination)?,
                Some(transaction.backup.clone())
            )
        }
        ExpectedRead::Error => {
            readable_path(&destination).expect_err(name);
        }
    }
    assert_eq!(files(&destination, &transaction)?, before, "{name}");

    match recovery {
        ExpectedRecovery::Success(expected_outcome, final_destination) => {
            assert_eq!(recover_interrupted_write(&destination)?, expected_outcome);
            assert_eq!(
                files(&destination, &transaction)?,
                Files {
                    destination: final_destination.map(<[u8]>::to_vec),
                    temp: None,
                    backup: None,
                    marker: None,
                },
                "{name}"
            );
        }
        ExpectedRecovery::Error => {
            recover_interrupted_write(&destination).expect_err(name);
            assert_eq!(files(&destination, &transaction)?, before, "{name}");
        }
    }
    Ok(())
}

#[test]
fn marked_transactions_use_only_verified_generations() -> anyhow::Result<()> {
    use ExpectedRead::Backup;
    use ExpectedRead::Destination;
    use ExpectedRead::Error;
    use ExpectedRead::Temp;
    use ExpectedRecovery::Error as RecoveryError;
    use ExpectedRecovery::Success;
    use RepairOutcome::Committed;
    use RepairOutcome::Unchanged;
    use TransactionKind::FirstPublish as First;
    use TransactionKind::ReplaceExisting as Replace;

    macro_rules! case {
        ($name:literal, $kind:expr, $destination:expr, $temp:expr, $backup:expr, $read:expr, $recovery:expr) => {
            Case($name, $kind, $destination, $temp, $backup, $read, $recovery)
        };
    }
    let cases = [
        case!(
            "first destination",
            First,
            Some(NEW),
            None,
            None,
            Destination,
            Success(Committed, Some(NEW))
        ),
        case!(
            "first temp",
            First,
            None,
            Some(NEW),
            None,
            Temp,
            Success(Committed, Some(NEW))
        ),
        case!("first empty", First, None, None, None, Error, RecoveryError),
        case!(
            "first destination temp",
            First,
            Some(NEW),
            Some(NEW),
            None,
            Error,
            RecoveryError
        ),
        case!(
            "replace source destination",
            Replace,
            Some(OLD),
            None,
            None,
            Destination,
            Success(Unchanged, Some(OLD))
        ),
        case!(
            "replace committed destination",
            Replace,
            Some(NEW),
            None,
            None,
            Destination,
            Success(Committed, Some(NEW))
        ),
        case!(
            "replace destination temp",
            Replace,
            Some(OLD),
            Some(NEW),
            None,
            Destination,
            Success(Unchanged, Some(OLD))
        ),
        case!(
            "replace destination backup",
            Replace,
            Some(NEW),
            None,
            Some(OLD),
            Destination,
            Success(Committed, Some(NEW))
        ),
        case!(
            "replace temp backup",
            Replace,
            None,
            Some(NEW),
            Some(OLD),
            Backup,
            Success(Unchanged, Some(OLD))
        ),
        case!(
            "replace backup only",
            Replace,
            None,
            None,
            Some(OLD),
            Error,
            RecoveryError
        ),
        case!(
            "replace temp only",
            Replace,
            None,
            Some(NEW),
            None,
            Error,
            RecoveryError
        ),
        case!(
            "replace all",
            Replace,
            Some(NEW),
            Some(NEW),
            Some(OLD),
            Error,
            RecoveryError
        ),
    ];
    for case in cases {
        run_case(case)?;
    }
    Ok(())
}

#[test]
fn mismatched_generation_is_preserved() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = stage(
        &destination,
        TransactionKind::ReplaceExisting,
        Some(b"foreign"),
        /*temp*/ None,
        Some(OLD),
    )?;
    let before = files(&destination, &transaction)?;

    readable_path(&destination).expect_err("foreign generation must not be selected");
    recover_interrupted_write(&destination).expect_err("foreign generation must be preserved");
    assert_eq!(files(&destination, &transaction)?, before);
    Ok(())
}

#[test]
fn invalid_markers_are_preserved() -> anyhow::Result<()> {
    for marker in [b"partial".as_slice(), ZERO_MARKER.as_slice()] {
        let dir = tempfile::tempdir()?;
        let destination = dir.path().join("local.age");
        let transaction =
            transaction_paths(&destination, TransactionKind::FirstPublish, "a1-b2-c3")?;
        fs::write(&transaction.temp, NEW)?;
        fs::write(&transaction.marker, marker)?;
        let before = files(&destination, &transaction)?;

        readable_path(&destination).expect_err("invalid marker must fail closed");
        recover_interrupted_write(&destination).expect_err("invalid marker must fail closed");
        assert_eq!(files(&destination, &transaction)?, before);
    }
    Ok(())
}

#[test]
fn unmarked_artifacts_are_ignored_and_preserved() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction =
        transaction_paths(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&destination, b"current")?;
    fs::write(&transaction.temp, NEW)?;
    fs::write(&transaction.backup, OLD)?;

    assert_eq!(readable_path(&destination)?, Some(destination.clone()));
    assert_eq!(
        recover_interrupted_write(&destination)?,
        RepairOutcome::Current
    );
    assert!(!recovery_artifacts_exist(&destination)?);
    assert_eq!(fs::read(&transaction.temp)?, NEW);
    assert_eq!(fs::read(&transaction.backup)?, OLD);
    Ok(())
}

#[test]
fn multiple_marked_transactions_are_preserved() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let first = transaction_paths(&destination, TransactionKind::FirstPublish, "a1-b2-c3")?;
    let second = transaction_paths(&destination, TransactionKind::FirstPublish, "d4-e5-f6")?;
    fs::write(&first.temp, NEW)?;
    fs::write(&second.temp, NEW)?;
    write_transaction_marker(&first, /*source*/ None, NEW)?;
    write_transaction_marker(&second, /*source*/ None, NEW)?;

    readable_path(&destination).expect_err("multiple transactions must fail closed");
    assert!(first.marker.try_exists()?);
    assert!(second.marker.try_exists()?);
    Ok(())
}

#[test]
fn failed_replace_recovers_only_documented_states() -> anyhow::Result<()> {
    for (destination_bytes, temp, backup, error_code, recovers) in [
        (Some(OLD), Some(NEW), None, 5, true),
        (None, Some(NEW), Some(OLD), 1177, true),
        (None, Some(NEW), Some(OLD), 5, false),
        (Some(NEW), None, Some(OLD), 1177, false),
    ] {
        let dir = tempfile::tempdir()?;
        let destination = dir.path().join("local.age");
        let transaction = stage(
            &destination,
            TransactionKind::ReplaceExisting,
            destination_bytes,
            temp,
            backup,
        )?;
        let before = files(&destination, &transaction)?;
        let error = std::io::Error::from_raw_os_error(error_code);

        if recovers {
            recover_failed_replace(&destination, &error)?;
            assert_eq!(
                files(&destination, &transaction)?,
                Files {
                    destination: Some(OLD.to_vec()),
                    temp: None,
                    backup: None,
                    marker: None,
                }
            );
        } else {
            recover_failed_replace(&destination, &error)
                .expect_err("undocumented failure state must be preserved");
            assert_eq!(files(&destination, &transaction)?, before);
        }
    }
    Ok(())
}

#[test]
fn failed_replace_preserves_destination_and_cleans_owned_artifacts() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("destination");
    fs::create_dir(&destination)?;

    write_file_atomically(&destination, b"secret").expect_err("replacing a directory must fail");

    assert!(destination.is_dir());
    let entries = fs::read_dir(dir.path())?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path(), destination);
    Ok(())
}

#[test]
fn atomic_write_replaces_contents_without_artifacts() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");

    write_file_atomically(&destination, b"one")?;
    write_file_atomically(&destination, b"two")?;

    assert_eq!(fs::read(&destination)?, b"two");
    assert!(!recovery_artifacts_exist(&destination)?);
    let entries = fs::read_dir(dir.path())?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path(), destination);
    Ok(())
}

#[cfg(unix)]
#[test]
fn atomic_write_hardens_permissions_on_create_and_replace() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");

    write_file_atomically(&destination, b"one")?;
    assert_eq!(
        fs::metadata(&destination)?.permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&destination, fs::Permissions::from_mode(/*mode*/ 0o644))?;
    write_file_atomically(&destination, b"two")?;

    assert_eq!(fs::read(&destination)?, b"two");
    assert_eq!(
        fs::metadata(destination)?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}
