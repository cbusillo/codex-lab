use std::fs;
use std::sync::Arc;

use codex_keyring_store::tests::MockKeyringStore;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn interrupted_replace_reads_backup_then_recovers_before_mutation() -> Result<()> {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let keyring = Arc::new(MockKeyringStore::default());
    let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring);
    let first_name = SecretName::new("FIRST")?;
    let second_name = SecretName::new("SECOND")?;
    backend.set(&SecretScope::Global, &first_name, "one")?;

    let path = backend.secrets_path();
    let old_ciphertext = fs::read(&path)?;
    backend.set(&SecretScope::Global, &first_name, "new")?;
    let new_ciphertext = fs::read(&path)?;
    let transaction = atomic_file::transaction_paths(
        &path,
        atomic_file::TransactionKind::ReplaceExisting,
        "a1-b2-c3",
    )?;
    fs::remove_file(&path)?;
    fs::write(&transaction.temp, &new_ciphertext)?;
    fs::write(&transaction.backup, &old_ciphertext)?;
    let marker = atomic_file::MarkerRecord::new(
        atomic_file::TransactionKind::ReplaceExisting,
        Some(&old_ciphertext),
        &new_ciphertext,
    )?;
    fs::write(
        &transaction.marker,
        marker.encode(atomic_file::TransactionKind::ReplaceExisting),
    )?;

    assert_eq!(
        backend.get(&SecretScope::Global, &first_name)?,
        Some("one".to_string())
    );
    assert!(transaction.temp.try_exists()?);
    assert!(transaction.backup.try_exists()?);
    assert!(transaction.marker.try_exists()?);

    backend.set(&SecretScope::Global, &second_name, "two")?;
    assert_eq!(
        backend.get(&SecretScope::Global, &first_name)?,
        Some("one".to_string())
    );
    assert_eq!(
        backend.get(&SecretScope::Global, &second_name)?,
        Some("two".to_string())
    );
    assert!(!transaction.marker.try_exists()?);
    Ok(())
}
