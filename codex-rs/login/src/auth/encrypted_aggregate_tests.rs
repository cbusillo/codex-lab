use std::fs;
use std::path::Path;
use std::sync::Arc;

use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AuthCredentialsStoreMode::Auto;
use codex_config::types::AuthCredentialsStoreMode::File;
use codex_config::types::AuthCredentialsStoreMode::Keyring;
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
use super::encrypted_aggregate::prepare_encrypted_aggregate;
use super::storage::auth_keyring_account_for_tests;
use super::storage::load_auth_with_keyring_store;
use super::storage::save_auth_with_keyring_store;
use crate::auth::AuthDotJson;

#[test]
fn prepare_records_source_and_preserves_legacy_credentials() -> anyhow::Result<()> {
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
        let document = prepare(temp.path(), mode, keyring.clone())?;
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
fn prepare_fails_closed_on_keyring_error() -> anyhow::Result<()> {
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
        let err = prepare_encrypted_aggregate(temp.path(), mode, keyring)
            .expect_err("keyring read failure must abort migration");
        assert!(mode == Keyring || err.to_string().contains("encrypted migration"));
        assert!(!temp.path().join("secrets/codex_auth.age").exists());
    }
    Ok(())
}

#[test]
fn access_token_prepares_are_idempotent_and_preserve_legacy() -> anyhow::Result<()> {
    for auth in [
        json!({ "personal_access_token": "at-example" }),
        json!({ "auth_mode": "agentIdentity", "agent_identity": "jwt" }),
    ] {
        let temp = TempDir::new()?;
        let keyring = Arc::new(MockKeyringStore::default());
        let auth_bytes = serde_json::to_vec_pretty(&auth)?;
        fs::write(temp.path().join("auth.json"), &auth_bytes)?;
        let accounts_bytes = seed_accounts_file(temp.path())?;
        let document = prepare(temp.path(), File, keyring.clone())?;
        assert_eq!(document.accounts.accounts.len(), 1);
        let encrypted_bytes = fs::read(temp.path().join("secrets/codex_auth.age"))?;
        let result = prepare_encrypted_aggregate(temp.path(), File, keyring)?;
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
    fs::write(temp.path().join("auth_accounts.json"), " \n")?;
    let result = prepare_encrypted_aggregate(temp.path(), File, keyring.clone())?;
    assert_eq!(result, PreparedMigration::Nothing);
    assert!(!temp.path().join("secrets/codex_auth.age").exists());
    let auth_bytes = seed_auth_file(temp.path())?;
    let accounts_bytes = seed_accounts_file(temp.path())?;
    let result =
        prepare_encrypted_aggregate(temp.path(), AuthCredentialsStoreMode::Ephemeral, keyring)?;
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
fn auth_only_prepares_default_accounts() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    seed_auth_file(temp.path())?;
    let document = prepare(temp.path(), AuthCredentialsStoreMode::File, keyring)?;
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
fn accounts_only_prepares_without_active_auth() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    seed_accounts_file(temp.path())?;
    let document = prepare(temp.path(), AuthCredentialsStoreMode::File, keyring)?;
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
    prepare_encrypted_aggregate(temp.path(), AuthCredentialsStoreMode::File, keyring.clone())?;
    fs::write(temp.path().join("secrets/codex_auth.age"), b"garbage")?;
    let err = prepare_encrypted_aggregate(temp.path(), AuthCredentialsStoreMode::File, keyring)
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
        let err = prepare_encrypted_aggregate(temp.path(), AuthCredentialsStoreMode::File, keyring)
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
    assert!(prepare_encrypted_aggregate(temp.path(), File, keyring.clone()).is_err());
    assert!(!encrypted_path.exists());
    seed_accounts_file(temp.path())?;
    prepare_encrypted_aggregate(temp.path(), File, keyring.clone())?;
    let encrypted_bytes = fs::read(&encrypted_path)?;
    fs::write(
        temp.path().join("auth.json"),
        serde_json::to_vec_pretty(&json!({
            "OPENAI_API_KEY": "sk-updated",
            "auth_mode": "apikey"
        }))?,
    )?;
    seed_accounts_file_with_key(temp.path(), "sk-updated")?;
    let err = prepare_encrypted_aggregate(temp.path(), File, keyring.clone())
        .expect_err("stale aggregate must fail");
    assert!(err.to_string().contains("current legacy sources"));
    fs::remove_file(temp.path().join("auth.json"))?;
    fs::remove_file(temp.path().join("auth_accounts.json"))?;
    let err = prepare_encrypted_aggregate(temp.path(), File, keyring)
        .expect_err("orphaned aggregate must fail");
    assert!(err.to_string().contains("current legacy sources"));
    assert_eq!(fs::read(encrypted_path)?, encrypted_bytes);
    Ok(())
}

fn prepare(
    home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring: Arc<MockKeyringStore>,
) -> anyhow::Result<LoginAggregateV1> {
    match prepare_encrypted_aggregate(home, mode, keyring)? {
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
    let raw = secrets_manager(home, keyring)
        .get(&SecretScope::Global, &aggregate_secret_name())?
        .expect("aggregate");
    Ok(serde_json::from_str(&raw)?)
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
