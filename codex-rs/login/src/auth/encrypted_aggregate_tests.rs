use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;

use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AuthCredentialsStoreMode::Auto;
use codex_config::types::AuthCredentialsStoreMode::File;
use codex_config::types::AuthCredentialsStoreMode::Keyring;
use codex_keyring_store::CredentialStoreError;
use codex_keyring_store::KeyringStore;
use codex_keyring_store::tests::MockKeyringStore;
use codex_secrets::LocalSecretsNamespace;
use codex_secrets::SecretName;
use codex_secrets::SecretScope;
use codex_secrets::SecretsBackendKind;
use codex_secrets::SecretsManager;
use keyring::Error as KeyringError;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::encrypted_aggregate::AggregateAuthSource;
use super::encrypted_aggregate::LOGIN_AGGREGATE_SECRET;
use super::encrypted_aggregate::LoginAggregateV1;
use super::encrypted_aggregate::PreparedMigration;
use super::encrypted_aggregate::activate_encrypted_aggregate;
use super::storage::auth_keyring_account_for_tests;
use super::storage::load_activated_auth_with_keyring_store;
use super::storage::load_auth_with_keyring_store;
use super::storage::save_activated_auth_with_keyring_store;
use super::storage::save_auth_with_keyring_store;
use crate::auth::AuthDotJson;
use crate::auth_accounts::read_accounts_file_for_migration;
use crate::auth_accounts::write_accounts_file_with_keyring_store_for_tests;

#[derive(Debug, Default)]
struct SaveFailingKeyringStore;

impl KeyringStore for SaveFailingKeyringStore {
    fn load(&self, _service: &str, _account: &str) -> Result<Option<String>, CredentialStoreError> {
        Ok(None)
    }

    fn save(
        &self,
        _service: &str,
        _account: &str,
        _value: &str,
    ) -> Result<(), CredentialStoreError> {
        Err(CredentialStoreError::new(KeyringError::Invalid(
            "activation".into(),
            "save".into(),
        )))
    }

    fn delete(&self, _service: &str, _account: &str) -> Result<bool, CredentialStoreError> {
        Ok(false)
    }
}

#[test]
fn activation_records_source_and_preserves_legacy_credentials() -> anyhow::Result<()> {
    for (mode, keyring_backed, api_key, source) in [
        (File, false, "sk-file", AggregateAuthSource::File),
        (Keyring, true, "sk-keyring", AggregateAuthSource::Keyring),
        (Auto, true, "sk-auto-keyring", AggregateAuthSource::Keyring),
        (Auto, false, "sk-file", AggregateAuthSource::File),
    ] {
        let temp = TempDir::new()?;
        let keyring = Arc::new(MockKeyringStore::default());
        let expected_auth = if keyring_backed {
            seed_keyring_auth(temp.path(), keyring.clone(), api_key)?
        } else {
            serde_json::from_slice::<AuthDotJson>(&seed_auth_file(temp.path())?)?
        };
        let auth_bytes = (!keyring_backed)
            .then(|| fs::read(temp.path().join("auth.json")))
            .transpose()?;
        let catalog_key = if mode == AuthCredentialsStoreMode::File {
            api_key
        } else {
            "sk-stale"
        };
        let accounts_bytes = seed_accounts_file_with_key(temp.path(), catalog_key)?;
        let document = activate(temp.path(), mode, keyring.clone())?;
        assert!(temp.path().join("secrets/codex_auth.age").exists());
        assert_eq!(document.provenance.store_mode, mode);
        assert_eq!(document.provenance.active_auth_source, Some(source));
        assert_eq!(document.active_auth.as_ref(), Some(&expected_auth));
        assert_eq!(
            document.accounts.active_account_id.as_deref(),
            Some("acct-file")
        );
        assert_eq!(load_aggregate(temp.path(), keyring.clone())?, document);
        assert_eq!(
            fs::read(temp.path().join("auth_accounts.json"))?,
            accounts_bytes
        );
        if let Some(auth_bytes) = auth_bytes {
            assert_eq!(fs::read(temp.path().join("auth.json"))?, auth_bytes);
        } else {
            assert_eq!(
                load_auth_with_keyring_store(
                    temp.path(),
                    AuthCredentialsStoreMode::Keyring,
                    keyring
                )?,
                Some(expected_auth)
            );
            assert!(!temp.path().join("auth.json").exists());
        }
    }
    Ok(())
}

#[test]
fn initial_activation_defers_on_keyring_error() -> anyhow::Result<()> {
    for mode in [Auto, Keyring] {
        let temp = TempDir::new()?;
        let keyring = Arc::new(MockKeyringStore::default());
        seed_auth_file(temp.path())?;
        seed_accounts_file(temp.path())?;
        let auth_key = auth_keyring_account_for_tests(temp.path())?;
        keyring.set_error(
            &auth_key,
            KeyringError::Invalid("migration".into(), "load".into()),
        );
        let result = activate_encrypted_aggregate(temp.path(), mode, keyring)?;
        assert_eq!(result, PreparedMigration::Deferred);
        assert!(!temp.path().join("secrets/codex_auth.age").exists());
    }
    Ok(())
}

#[test]
fn access_token_activation_is_idempotent_and_preserves_legacy() -> anyhow::Result<()> {
    for auth in [
        json!({ "personal_access_token": "at-example" }),
        json!({ "auth_mode": "agentIdentity", "agent_identity": "jwt" }),
    ] {
        let temp = TempDir::new()?;
        let keyring = Arc::new(MockKeyringStore::default());
        let auth_bytes = serde_json::to_vec_pretty(&auth)?;
        fs::write(temp.path().join("auth.json"), &auth_bytes)?;
        let accounts_bytes = seed_accounts_file(temp.path())?;
        let document = activate(temp.path(), File, keyring.clone())?;
        assert_eq!(document.accounts.accounts.len(), 1);
        let encrypted_bytes = fs::read(temp.path().join("secrets/codex_auth.age"))?;
        let result = activate_encrypted_aggregate(temp.path(), File, keyring)?;
        assert_eq!(result, PreparedMigration::AlreadyEncrypted(document));
        assert_eq!(
            fs::read(temp.path().join("secrets/codex_auth.age"))?,
            encrypted_bytes
        );
        assert_eq!(fs::read(temp.path().join("auth.json"))?, auth_bytes);
        assert_eq!(
            fs::read(temp.path().join("auth_accounts.json"))?,
            accounts_bytes
        );
    }
    Ok(())
}

#[test]
fn no_source_and_ephemeral_paths_do_not_write() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    fs::write(temp.path().join("auth_accounts.json"), "{\"version\":1}")?;
    let result = activate_encrypted_aggregate(temp.path(), File, keyring.clone())?;
    assert_eq!(result, PreparedMigration::Nothing);
    assert!(!temp.path().join("secrets/codex_auth.age").exists());
    let auth_bytes = seed_auth_file(temp.path())?;
    let accounts_bytes = seed_accounts_file(temp.path())?;
    let result =
        activate_encrypted_aggregate(temp.path(), AuthCredentialsStoreMode::Ephemeral, keyring)?;
    assert_eq!(result, PreparedMigration::Nothing);
    assert!(!temp.path().join("secrets/codex_auth.age").exists());
    assert_eq!(fs::read(temp.path().join("auth.json"))?, auth_bytes);
    assert_eq!(
        fs::read(temp.path().join("auth_accounts.json"))?,
        accounts_bytes
    );
    Ok(())
}

#[test]
fn auth_only_activation_uses_default_accounts() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    seed_auth_file(temp.path())?;
    let document = activate(temp.path(), AuthCredentialsStoreMode::File, keyring)?;
    assert_eq!(
        document.provenance.active_auth_source,
        Some(AggregateAuthSource::File)
    );
    assert!(!document.provenance.catalog_present);
    assert_eq!(document.accounts.active_account_id, None);
    assert!(document.accounts.accounts.is_empty());
    assert!(!temp.path().join("auth_accounts.json").exists());
    Ok(())
}

#[test]
fn accounts_only_activation_works_without_active_auth() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    seed_accounts_file(temp.path())?;
    let document = activate(temp.path(), AuthCredentialsStoreMode::File, keyring)?;
    assert_eq!(document.provenance.active_auth_source, None);
    assert!(document.provenance.catalog_present);
    assert_eq!(document.active_auth, None);
    assert_eq!(
        document.accounts.active_account_id.as_deref(),
        Some("acct-file")
    );
    Ok(())
}

#[test]
fn corrupt_encrypted_aggregate_fails_closed() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    let auth_bytes = seed_auth_file(temp.path())?;
    let accounts_bytes = seed_accounts_file(temp.path())?;
    activate_encrypted_aggregate(temp.path(), AuthCredentialsStoreMode::File, keyring.clone())?;
    fs::write(temp.path().join("secrets/codex_auth.age"), b"garbage")?;
    let err = activate_encrypted_aggregate(temp.path(), AuthCredentialsStoreMode::File, keyring)
        .expect_err("corrupt encrypted aggregate must fail");
    assert!(err.to_string().contains("failed"));
    assert_eq!(
        fs::read(temp.path().join("secrets/codex_auth.age"))?,
        b"garbage"
    );
    assert_eq!(fs::read(temp.path().join("auth.json"))?, auth_bytes);
    assert_eq!(
        fs::read(temp.path().join("auth_accounts.json"))?,
        accounts_bytes
    );
    Ok(())
}

#[test]
fn invalid_existing_documents_are_rejected() -> anyhow::Result<()> {
    let cases = [
        (
            json!({
                "version": 2,
                "provenance": base_provenance(),
                "accounts": { "version": 1 }
            }),
            "unsupported encrypted login aggregate version 2",
        ),
        (
            json!({
                "version": 1,
                "provenance": base_provenance(),
                "accounts": { "version": 2 }
            }),
            "unsupported encrypted login aggregate accounts version 2",
        ),
        (
            json!({
                "version": 1,
                "provenance": {
                    "store_mode": "file",
                    "active_auth_source": "file",
                    "catalog_present": true,
                    "assembled_from": "legacy_migration"
                },
                "accounts": { "version": 1 }
            }),
            "active auth provenance is inconsistent",
        ),
        (
            json!({
                "version": 1,
                "provenance": base_provenance(),
                "accounts": { "version": 1 },
                "unexpected": true
            }),
            "unknown field",
        ),
    ];
    for (document, expected_error) in cases {
        let temp = TempDir::new()?;
        let keyring = Arc::new(MockKeyringStore::default());
        store_raw_aggregate(temp.path(), keyring.clone(), document)?;
        let err =
            activate_encrypted_aggregate(temp.path(), AuthCredentialsStoreMode::File, keyring)
                .expect_err("invalid aggregate must fail");
        assert!(err.to_string().contains(expected_error), "{err}");
    }
    Ok(())
}

#[test]
fn stale_or_orphaned_aggregate_fails_without_overwrite() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    seed_auth_file(temp.path())?;
    seed_accounts_file_with_key(temp.path(), "sk-mismatch")?;
    let encrypted_path = temp.path().join("secrets/codex_auth.age");
    assert_eq!(
        activate_encrypted_aggregate(temp.path(), File, keyring.clone())?,
        PreparedMigration::Deferred
    );
    assert!(!encrypted_path.exists());
    seed_accounts_file(temp.path())?;
    activate_encrypted_aggregate(temp.path(), File, keyring.clone())?;
    let encrypted_bytes = fs::read(&encrypted_path)?;
    fs::write(
        temp.path().join("auth.json"),
        serde_json::to_vec_pretty(&json!({
            "OPENAI_API_KEY": "sk-updated",
            "auth_mode": "apikey"
        }))?,
    )?;
    seed_accounts_file_with_key(temp.path(), "sk-updated")?;
    let err = activate_encrypted_aggregate(temp.path(), File, keyring.clone())
        .expect_err("stale aggregate must fail");
    assert!(err.to_string().contains("current legacy sources"));
    fs::remove_file(temp.path().join("auth.json"))?;
    fs::remove_file(temp.path().join("auth_accounts.json"))?;
    let err = activate_encrypted_aggregate(temp.path(), File, keyring)
        .expect_err("orphaned aggregate must fail");
    assert!(err.to_string().contains("current legacy sources"));
    assert_eq!(fs::read(encrypted_path)?, encrypted_bytes);
    Ok(())
}

#[test]
fn production_load_activates_verified_shadow_without_changing_legacy() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    let auth_bytes = seed_auth_file(temp.path())?;
    let accounts_bytes = seed_accounts_file(temp.path())?;

    let loaded = load_activated_auth_with_keyring_store(temp.path(), File, keyring.clone())?
        .expect("legacy auth should load");

    assert_eq!(loaded.openai_api_key.as_deref(), Some("sk-file"));
    assert_eq!(
        load_aggregate(temp.path(), keyring)?.active_auth,
        Some(loaded)
    );
    assert_eq!(fs::read(temp.path().join("auth.json"))?, auth_bytes);
    assert_eq!(
        fs::read(temp.path().join("auth_accounts.json"))?,
        accounts_bytes
    );
    Ok(())
}

#[test]
fn inconsistent_initial_legacy_state_defers_activation() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    fs::write(
        temp.path().join("auth.json"),
        serde_json::to_vec_pretty(&json!({
            "OPENAI_API_KEY": "sk-imported",
            "auth_mode": "apikey"
        }))?,
    )?;
    seed_accounts_file(temp.path())?;

    let loaded = load_activated_auth_with_keyring_store(temp.path(), File, keyring.clone())?
        .expect("legacy auth should load during benign drift");

    assert_eq!(loaded.openai_api_key.as_deref(), Some("sk-imported"));
    assert!(load_raw_aggregate(temp.path(), keyring)?.is_none());
    Ok(())
}

#[test]
fn auto_activation_defers_when_keyring_fallback_is_needed() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    seed_auth_file(temp.path())?;
    let auth_key = auth_keyring_account_for_tests(temp.path())?;
    keyring.set_error(
        &auth_key,
        KeyringError::Invalid("activation".into(), "load".into()),
    );

    let loaded = load_activated_auth_with_keyring_store(temp.path(), Auto, keyring.clone())?
        .expect("Auto should preserve its file fallback");

    assert_eq!(loaded.openai_api_key.as_deref(), Some("sk-file"));
    assert!(load_raw_aggregate(temp.path(), keyring)?.is_none());
    Ok(())
}

#[test]
fn initial_activation_write_failure_preserves_legacy_load() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    seed_auth_file(temp.path())?;
    seed_accounts_file(temp.path())?;
    let keyring_store: Arc<dyn KeyringStore> = Arc::new(SaveFailingKeyringStore);

    let loaded = load_activated_auth_with_keyring_store(temp.path(), File, keyring_store)?
        .expect("legacy auth should load when shadow creation fails");

    assert_eq!(loaded.openai_api_key.as_deref(), Some("sk-file"));
    assert!(!temp.path().join("secrets/codex_auth.age").exists());
    Ok(())
}

#[test]
fn established_auto_shadow_preserves_keyring_file_fallback() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    seed_auth_file(temp.path())?;
    let initial = load_activated_auth_with_keyring_store(temp.path(), Auto, keyring.clone())?
        .expect("Auto should activate from its file fallback");
    assert_eq!(initial.openai_api_key.as_deref(), Some("sk-file"));
    assert!(load_raw_aggregate(temp.path(), keyring.clone())?.is_some());

    let auth_key = auth_keyring_account_for_tests(temp.path())?;
    keyring.set_error(
        &auth_key,
        KeyringError::Invalid("activation".into(), "load".into()),
    );
    let loaded = load_activated_auth_with_keyring_store(temp.path(), Auto, keyring.clone())?
        .expect("established shadow should preserve Auto file fallback");

    assert_eq!(loaded.openai_api_key.as_deref(), Some("sk-file"));
    assert!(load_raw_aggregate(temp.path(), keyring)?.is_some());
    Ok(())
}

#[test]
fn trusted_writes_invalidate_shadow_until_next_load() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    seed_auth_file(temp.path())?;
    seed_accounts_file(temp.path())?;
    load_activated_auth_with_keyring_store(temp.path(), File, keyring.clone())?
        .expect("legacy auth should load");

    let updated_auth: AuthDotJson = serde_json::from_value(json!({
        "OPENAI_API_KEY": "sk-updated",
        "auth_mode": "apikey"
    }))?;
    save_activated_auth_with_keyring_store(temp.path(), &updated_auth, File, keyring.clone())?;
    assert!(load_raw_aggregate(temp.path(), keyring.clone())?.is_none());

    let (mut accounts, catalog_present) = read_accounts_file_for_migration(temp.path())?;
    assert!(catalog_present);
    accounts.accounts[0].openai_api_key = Some("sk-updated".to_string());
    write_accounts_file_with_keyring_store_for_tests(
        temp.path(),
        File,
        &accounts,
        keyring.clone(),
    )?;
    assert!(load_raw_aggregate(temp.path(), keyring.clone())?.is_none());

    assert_eq!(
        load_activated_auth_with_keyring_store(temp.path(), File, keyring.clone())?,
        Some(updated_auth.clone())
    );
    let aggregate = load_aggregate(temp.path(), keyring)?;
    assert_eq!(aggregate.active_auth, Some(updated_auth));
    assert_eq!(aggregate.accounts, accounts);
    Ok(())
}

#[test]
fn concurrent_load_and_write_do_not_report_shadow_races() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    seed_auth_file(temp.path())?;
    seed_accounts_file(temp.path())?;
    load_activated_auth_with_keyring_store(temp.path(), File, keyring.clone())?
        .expect("legacy auth should load");

    let barrier = Arc::new(Barrier::new(2));
    let writer_home = temp.path().to_path_buf();
    let writer_keyring = keyring.clone();
    let writer_barrier = barrier.clone();
    let writer = thread::spawn(move || {
        let updated_auth: AuthDotJson = serde_json::from_value(json!({
            "OPENAI_API_KEY": "sk-concurrent",
            "auth_mode": "apikey"
        }))?;
        writer_barrier.wait();
        save_activated_auth_with_keyring_store(&writer_home, &updated_auth, File, writer_keyring)
    });

    barrier.wait();
    let loaded = load_activated_auth_with_keyring_store(temp.path(), File, keyring.clone())?
        .expect("concurrent legacy auth should load");
    writer.join().expect("writer should not panic")?;

    assert!(matches!(
        loaded.openai_api_key.as_deref(),
        Some("sk-file" | "sk-concurrent")
    ));
    assert!(load_raw_aggregate(temp.path(), keyring)?.is_none());
    Ok(())
}

#[test]
fn corrupt_shadow_blocks_reads_and_trusted_mutations() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    let auth_bytes = seed_auth_file(temp.path())?;
    let accounts_bytes = seed_accounts_file(temp.path())?;
    load_activated_auth_with_keyring_store(temp.path(), File, keyring.clone())?
        .expect("legacy auth should load");
    fs::write(temp.path().join("secrets/codex_auth.age"), b"garbage")?;

    let updated_auth: AuthDotJson = serde_json::from_value(json!({
        "OPENAI_API_KEY": "sk-updated",
        "auth_mode": "apikey"
    }))?;
    assert!(
        save_activated_auth_with_keyring_store(temp.path(), &updated_auth, File, keyring.clone())
            .is_err()
    );
    let (mut accounts, _) = read_accounts_file_for_migration(temp.path())?;
    accounts.accounts[0].label = Some("updated".to_string());
    assert!(
        write_accounts_file_with_keyring_store_for_tests(
            temp.path(),
            File,
            &accounts,
            keyring.clone(),
        )
        .is_err()
    );
    assert!(load_activated_auth_with_keyring_store(temp.path(), File, keyring).is_err());
    assert_eq!(fs::read(temp.path().join("auth.json"))?, auth_bytes);
    assert_eq!(
        fs::read(temp.path().join("auth_accounts.json"))?,
        accounts_bytes
    );
    Ok(())
}

fn activate(
    home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring: Arc<MockKeyringStore>,
) -> anyhow::Result<LoginAggregateV1> {
    match activate_encrypted_aggregate(home, mode, keyring)? {
        PreparedMigration::Prepared(document) => Ok(document),
        result => anyhow::bail!("expected prepared migration, got {result:?}"),
    }
}

fn base_provenance() -> serde_json::Value {
    json!({
        "store_mode": "file",
        "catalog_present": true,
        "assembled_from": "legacy_migration"
    })
}

fn seed_auth_file(home: &Path) -> anyhow::Result<Vec<u8>> {
    fs::create_dir_all(home)?;
    let auth = json!({ "OPENAI_API_KEY": "sk-file", "auth_mode": "apikey" });
    let bytes = serde_json::to_vec_pretty(&auth)?;
    fs::write(home.join("auth.json"), &bytes)?;
    Ok(bytes)
}

fn seed_keyring_auth(
    home: &Path,
    keyring: Arc<MockKeyringStore>,
    api_key: &str,
) -> anyhow::Result<AuthDotJson> {
    let auth = AuthDotJson {
        auth_mode: Some(codex_app_server_protocol::AuthMode::ApiKey),
        openai_api_key: Some(api_key.to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
    };
    save_auth_with_keyring_store(home, &auth, AuthCredentialsStoreMode::Keyring, keyring)?;
    Ok(auth)
}

fn seed_accounts_file(home: &Path) -> anyhow::Result<Vec<u8>> {
    let current = seed_accounts_file_with_key(home, "sk-file")?;
    let mut bytes = b"{\"version\":1}".to_vec();
    bytes.extend_from_slice(&current);
    fs::write(home.join("auth_accounts.json"), &bytes)?;
    Ok(bytes)
}

fn seed_accounts_file_with_key(home: &Path, api_key: &str) -> anyhow::Result<Vec<u8>> {
    fs::create_dir_all(home)?;
    let accounts = json!({
        "version": 1,
        "active_account_id": "acct-file",
        "accounts": [{
            "id": "acct-file",
            "mode": "apikey",
            "label": "File account",
            "openai_api_key": api_key,
            "last_refresh": "2025-01-01T00:00:00Z"
        }]
    });
    let bytes = serde_json::to_vec_pretty(&accounts)?;
    fs::write(home.join("auth_accounts.json"), &bytes)?;
    Ok(bytes)
}

fn load_aggregate(home: &Path, keyring: Arc<MockKeyringStore>) -> anyhow::Result<LoginAggregateV1> {
    let raw = load_raw_aggregate(home, keyring)?.expect("aggregate");
    Ok(serde_json::from_str(&raw)?)
}

fn load_raw_aggregate(
    home: &Path,
    keyring: Arc<MockKeyringStore>,
) -> anyhow::Result<Option<String>> {
    secrets_manager(home, keyring).get(&SecretScope::Global, &aggregate_secret_name())
}

fn store_raw_aggregate(
    home: &Path,
    keyring: Arc<MockKeyringStore>,
    document: serde_json::Value,
) -> anyhow::Result<()> {
    secrets_manager(home, keyring).set(
        &SecretScope::Global,
        &aggregate_secret_name(),
        &serde_json::to_string(&document)?,
    )?;
    Ok(())
}

fn secrets_manager(home: &Path, keyring: Arc<MockKeyringStore>) -> SecretsManager {
    SecretsManager::new_with_keyring_store_and_namespace(
        home.to_path_buf(),
        SecretsBackendKind::Local,
        keyring,
        LocalSecretsNamespace::CodexAuth,
    )
}

fn aggregate_secret_name() -> SecretName {
    SecretName::new(LOGIN_AGGREGATE_SECRET).expect("valid secret name")
}
