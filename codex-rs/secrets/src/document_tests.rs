use std::sync::Arc;

use codex_keyring_store::KeyringStore;
use codex_keyring_store::tests::MockKeyringStore;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde::Serialize;

use super::*;

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct TestDocument {
    value: String,
}

impl EncryptedDocument for TestDocument {
    const NAME: &'static str = "TEST_DOCUMENT";
    const SCHEMA_VERSION: u32 = 1;
}

fn test_store(codex_home: &std::path::Path, keyring: &MockKeyringStore) -> EncryptedDocumentStore {
    EncryptedDocumentStore::new_with_keyring_store(
        codex_home.to_path_buf(),
        LocalSecretsNamespace::CodexAuth,
        Arc::new(keyring.clone()),
    )
}

#[test]
fn missing_document_does_not_create_storage_or_key() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let keyring = MockKeyringStore::default();
    let store = test_store(codex_home.path(), &keyring);

    assert_eq!(store.load::<TestDocument>()?, None);
    let account = crate::compute_keyring_account_for_namespace(
        codex_home.path(),
        LocalSecretsNamespace::CodexAuth,
    );
    assert!(!keyring.contains(&account));
    assert!(!codex_home.path().join("secrets/codex_auth.age").exists());
    Ok(())
}

#[test]
fn document_round_trips_and_deletes() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let keyring = MockKeyringStore::default();
    let store = test_store(codex_home.path(), &keyring);
    let expected = TestDocument {
        value: "secret".to_string(),
    };

    store.store(&expected)?;
    assert_eq!(store.load::<TestDocument>()?, Some(expected));
    assert!(store.delete::<TestDocument>()?);
    assert_eq!(store.load::<TestDocument>()?, None);
    Ok(())
}

#[test]
fn missing_key_for_existing_ciphertext_fails_without_replacement() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let keyring = MockKeyringStore::default();
    let store = test_store(codex_home.path(), &keyring);
    store.store(&TestDocument {
        value: "secret".to_string(),
    })?;
    let encrypted_path = codex_home.path().join("secrets/codex_auth.age");
    let ciphertext = std::fs::read(&encrypted_path)?;
    let account = crate::compute_keyring_account_for_namespace(
        codex_home.path(),
        LocalSecretsNamespace::CodexAuth,
    );
    assert!(keyring.delete(crate::keyring_service(), &account)?);

    store
        .load::<TestDocument>()
        .expect_err("missing encryption key must fail closed");
    assert!(!keyring.contains(&account));
    assert_eq!(std::fs::read(encrypted_path)?, ciphertext);
    Ok(())
}

#[test]
fn malformed_or_newer_envelope_fails_closed() -> anyhow::Result<()> {
    let codex_home = tempfile::tempdir()?;
    let keyring = MockKeyringStore::default();
    let manager = SecretsManager::new_with_keyring_store_and_namespace(
        codex_home.path().to_path_buf(),
        SecretsBackendKind::Local,
        Arc::new(keyring.clone()),
        LocalSecretsNamespace::CodexAuth,
    );
    let name = SecretName::new(TestDocument::NAME)?;
    manager.set(&SecretScope::Global, &name, "not json")?;
    let store = test_store(codex_home.path(), &keyring);
    store
        .load::<TestDocument>()
        .expect_err("malformed envelope must fail closed");

    manager.set(
        &SecretScope::Global,
        &name,
        r#"{"schema_version":2,"payload":{"value":"future"}}"#,
    )?;
    store
        .load::<TestDocument>()
        .expect_err("newer envelope must fail closed");
    Ok(())
}
