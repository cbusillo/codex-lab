use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;

use super::MarkerRecord;
use super::TransactionKind;
use super::TransactionPaths;
use super::readable_path;
use super::transaction_paths;
use super::write_file_atomically;

const OLD: &[u8] = b"old";
const NEW: &[u8] = b"new";
const FOREIGN: &[u8] = b"foreign";
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
struct Case(
    &'static str,
    TransactionKind,
    Bytes,
    Bytes,
    Bytes,
    ExpectedRead,
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
    let marker = MarkerRecord::new(kind, source, NEW)?.encode(kind);
    fs::write(&transaction.marker, marker)?;
    Ok(transaction)
}

fn run_case(case: Case) -> anyhow::Result<()> {
    let Case(name, kind, destination_bytes, temp, backup, expected) = case;
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = stage(&destination, kind, destination_bytes, temp, backup)?;
    let before = files(&destination, &transaction)?;

    match expected {
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
    Ok(())
}

#[test]
fn marked_transactions_select_only_complete_generations() -> anyhow::Result<()> {
    use ExpectedRead::Backup;
    use ExpectedRead::Destination;
    use ExpectedRead::Error;
    use ExpectedRead::Temp;
    use TransactionKind::FirstPublish as First;
    use TransactionKind::ReplaceExisting as Replace;

    macro_rules! case {
        ($name:literal, $kind:expr, $destination:expr, $temp:expr, $backup:expr, $read:expr) => {
            Case($name, $kind, $destination, $temp, $backup, $read)
        };
    }
    let cases = [
        case!(
            "first destination",
            First,
            Some(NEW),
            None,
            None,
            Destination
        ),
        case!("first temp", First, None, Some(NEW), None, Temp),
        case!("first empty", First, None, None, None, Error),
        case!("first backup", First, None, None, Some(OLD), Error),
        case!(
            "first destination temp",
            First,
            Some(NEW),
            Some(NEW),
            None,
            Error
        ),
        case!(
            "first destination backup",
            First,
            Some(NEW),
            None,
            Some(OLD),
            Error
        ),
        case!(
            "first temp backup",
            First,
            None,
            Some(NEW),
            Some(OLD),
            Error
        ),
        case!("first all", First, Some(NEW), Some(NEW), Some(OLD), Error),
        case!(
            "replace source destination",
            Replace,
            Some(OLD),
            None,
            None,
            Destination
        ),
        case!(
            "replace committed destination",
            Replace,
            Some(NEW),
            None,
            None,
            Destination
        ),
        case!(
            "replace destination temp",
            Replace,
            Some(OLD),
            Some(NEW),
            None,
            Destination
        ),
        case!(
            "replace destination backup",
            Replace,
            Some(NEW),
            None,
            Some(OLD),
            Destination
        ),
        case!(
            "replace temp backup",
            Replace,
            None,
            Some(NEW),
            Some(OLD),
            Backup
        ),
        case!("replace empty", Replace, None, None, None, Error),
        case!("replace temp", Replace, None, Some(NEW), None, Error),
        case!("replace backup", Replace, None, None, Some(OLD), Error),
        case!(
            "replace all",
            Replace,
            Some(NEW),
            Some(NEW),
            Some(OLD),
            Error
        ),
    ];
    for case in cases {
        run_case(case)?;
    }
    Ok(())
}

#[test]
fn mismatched_generations_are_preserved() -> anyhow::Result<()> {
    use TransactionKind::FirstPublish as First;
    use TransactionKind::ReplaceExisting as Replace;

    for (kind, destination_bytes, temp, backup) in [
        (First, Some(FOREIGN), None, None),
        (First, None, Some(FOREIGN), None),
        (Replace, Some(FOREIGN), None, None),
        (Replace, Some(OLD), Some(FOREIGN), None),
        (Replace, Some(NEW), None, Some(FOREIGN)),
        (Replace, None, Some(FOREIGN), Some(OLD)),
        (Replace, None, Some(NEW), Some(FOREIGN)),
    ] {
        let dir = tempfile::tempdir()?;
        let destination = dir.path().join("local.age");
        let transaction = stage(&destination, kind, destination_bytes, temp, backup)?;
        let before = files(&destination, &transaction)?;

        readable_path(&destination).expect_err("foreign generation must not be selected");
        assert_eq!(files(&destination, &transaction)?, before);
    }
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
    let marker = MarkerRecord::new(TransactionKind::FirstPublish, /*source*/ None, NEW)?;
    fs::write(&first.marker, marker.encode(TransactionKind::FirstPublish))?;
    fs::write(&second.marker, marker.encode(TransactionKind::FirstPublish))?;

    readable_path(&destination).expect_err("multiple transactions must fail closed");
    assert!(first.marker.try_exists()?);
    assert!(second.marker.try_exists()?);
    Ok(())
}

#[test]
fn atomic_write_replaces_contents_without_artifacts() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");

    write_file_atomically(&destination, b"one")?;
    write_file_atomically(&destination, b"two")?;

    assert_eq!(fs::read(&destination)?, b"two".to_vec());
    let entries = fs::read_dir(dir.path())?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path(), destination);
    Ok(())
}

#[cfg(not(windows))]
#[test]
fn failed_replace_preserves_destination_and_cleans_temp() -> anyhow::Result<()> {
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

    assert_eq!(fs::read(&destination)?, b"two".to_vec());
    assert_eq!(
        fs::metadata(destination)?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}
