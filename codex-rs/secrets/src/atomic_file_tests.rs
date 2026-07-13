use std::fs;

use pretty_assertions::assert_eq;

use super::RepairOutcome;
use super::TransactionKind;
use super::readable_path;
use super::recover_failed_replace;
use super::recover_interrupted_write;
use super::recovery_artifacts_exist;
use super::transaction_paths;
use super::write_file_atomically;

fn mark_transaction(
    destination: &std::path::Path,
    kind: TransactionKind,
    id: &str,
) -> anyhow::Result<super::TransactionPaths> {
    let transaction = transaction_paths(destination, kind, id)?;
    fs::write(&transaction.marker, b"")?;
    Ok(transaction)
}

#[test]
fn unmarked_artifacts_are_ignored_and_preserved() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction =
        transaction_paths(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&destination, b"current")?;
    fs::write(&transaction.temp, b"new")?;
    fs::write(&transaction.backup, b"old")?;

    assert_eq!(readable_path(&destination)?, Some(destination.clone()));
    assert_eq!(
        recover_interrupted_write(&destination)?,
        RepairOutcome::Current
    );
    assert!(!recovery_artifacts_exist(&destination)?);
    assert_eq!(fs::read(&transaction.temp)?, b"new".to_vec());
    assert_eq!(fs::read(&transaction.backup)?, b"old".to_vec());
    Ok(())
}

#[test]
fn marked_first_publish_is_readable_and_promoted() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::FirstPublish, "a1-b2-c3")?;
    fs::write(&transaction.temp, b"new")?;

    assert_eq!(readable_path(&destination)?, Some(transaction.temp));
    assert_eq!(
        recover_interrupted_write(&destination)?,
        RepairOutcome::Committed
    );
    assert_eq!(fs::read(&destination)?, b"new".to_vec());
    assert!(!recovery_artifacts_exist(&destination)?);
    Ok(())
}

#[test]
fn destination_and_temp_restore_unchanged_state() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&destination, b"old")?;
    fs::write(&transaction.temp, b"new")?;

    assert_eq!(readable_path(&destination)?, Some(destination.clone()));
    assert_eq!(
        recover_interrupted_write(&destination)?,
        RepairOutcome::Unchanged
    );
    assert_eq!(fs::read(&destination)?, b"old".to_vec());
    assert!(!recovery_artifacts_exist(&destination)?);
    Ok(())
}

#[test]
fn failed_replace_cleans_documented_unchanged_state() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&destination, b"old")?;
    fs::write(&transaction.temp, b"new")?;

    recover_failed_replace(&destination)?;

    assert_eq!(fs::read(&destination)?, b"old".to_vec());
    assert!(!recovery_artifacts_exist(&destination)?);
    Ok(())
}

#[test]
fn destination_and_backup_preserve_committed_state() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&destination, b"new")?;
    fs::write(&transaction.backup, b"old")?;

    assert_eq!(readable_path(&destination)?, Some(destination.clone()));
    assert_eq!(
        recover_interrupted_write(&destination)?,
        RepairOutcome::Committed
    );
    assert_eq!(fs::read(&destination)?, b"new".to_vec());
    assert!(!recovery_artifacts_exist(&destination)?);
    Ok(())
}

#[test]
fn temp_and_backup_restore_original_state() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&transaction.temp, b"new")?;
    fs::write(&transaction.backup, b"old")?;

    assert_eq!(readable_path(&destination)?, Some(transaction.backup));
    assert_eq!(
        recover_interrupted_write(&destination)?,
        RepairOutcome::Unchanged
    );
    assert_eq!(fs::read(&destination)?, b"old".to_vec());
    assert!(!recovery_artifacts_exist(&destination)?);
    Ok(())
}

#[test]
fn failed_replace_restores_documented_backup_state() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&transaction.temp, b"new")?;
    fs::write(&transaction.backup, b"old")?;

    recover_failed_replace(&destination)?;

    assert_eq!(fs::read(&destination)?, b"old".to_vec());
    assert!(!recovery_artifacts_exist(&destination)?);
    Ok(())
}

#[test]
fn backup_only_restores_original_state() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&transaction.backup, b"old")?;

    assert_eq!(readable_path(&destination)?, Some(transaction.backup));
    assert_eq!(
        recover_interrupted_write(&destination)?,
        RepairOutcome::Unchanged
    );
    assert_eq!(fs::read(&destination)?, b"old".to_vec());
    assert!(!recovery_artifacts_exist(&destination)?);
    Ok(())
}

#[test]
fn replacement_temp_only_is_indeterminate_and_preserved() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&transaction.temp, b"new")?;

    readable_path(&destination).expect_err("state must be rejected");
    recover_interrupted_write(&destination).expect_err("state must be rejected");
    assert_eq!(fs::read(&transaction.temp)?, b"new".to_vec());
    assert!(transaction.marker.try_exists()?);
    Ok(())
}

#[test]
fn indeterminate_state_preserves_all_generations() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&destination, b"current")?;
    fs::write(&transaction.temp, b"new")?;
    fs::write(&transaction.backup, b"old")?;

    let error = recover_interrupted_write(&destination).expect_err("state must be rejected");
    assert!(error.to_string().contains("indeterminate"));
    assert_eq!(fs::read(&destination)?, b"current".to_vec());
    assert_eq!(fs::read(&transaction.temp)?, b"new".to_vec());
    assert_eq!(fs::read(&transaction.backup)?, b"old".to_vec());
    assert!(transaction.marker.try_exists()?);
    Ok(())
}

#[test]
fn failed_replace_rejects_undocumented_committed_topology() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let transaction = mark_transaction(&destination, TransactionKind::ReplaceExisting, "a1-b2-c3")?;
    fs::write(&destination, b"new")?;
    fs::write(&transaction.backup, b"old")?;

    recover_failed_replace(&destination).expect_err("state must be rejected");

    assert_eq!(fs::read(&destination)?, b"new".to_vec());
    assert_eq!(fs::read(&transaction.backup)?, b"old".to_vec());
    assert!(transaction.marker.try_exists()?);
    Ok(())
}

#[test]
fn multiple_marked_transactions_are_preserved() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");
    let first = mark_transaction(&destination, TransactionKind::FirstPublish, "a1-b2-c3")?;
    let second = mark_transaction(&destination, TransactionKind::FirstPublish, "d4-e5-f6")?;
    fs::write(&first.temp, b"one")?;
    fs::write(&second.temp, b"two")?;

    let error = readable_path(&destination).expect_err("state must be rejected");
    assert!(error.to_string().contains("multiple secrets transactions"));
    assert!(first.marker.try_exists()?);
    assert!(second.marker.try_exists()?);
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
    assert_eq!(
        entries
            .into_iter()
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>(),
        vec![
            destination
                .file_name()
                .expect("destination filename")
                .to_owned()
        ]
    );
    Ok(())
}

#[test]
fn atomic_write_replaces_contents_without_artifacts() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let destination = dir.path().join("local.age");

    write_file_atomically(&destination, b"one")?;
    write_file_atomically(&destination, b"two")?;

    assert_eq!(fs::read(&destination)?, b"two".to_vec());
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

    assert_eq!(fs::read(&destination)?, b"two".to_vec());
    assert_eq!(
        fs::metadata(destination)?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}
