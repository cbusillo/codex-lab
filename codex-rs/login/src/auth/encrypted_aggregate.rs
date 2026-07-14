use std::io;
use std::path::Path;
use std::sync::Arc;

use codex_config::types::AuthCredentialsStoreMode;
use codex_keyring_store::KeyringStore;
use codex_secrets::LocalSecretsNamespace;
use codex_secrets::SecretMutation;
use codex_secrets::SecretName;
use codex_secrets::SecretScope;
use codex_secrets::SecretsBackendKind;
use codex_secrets::SecretsManager;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::auth::AuthDotJson;
use crate::auth::storage::AuthStorageSource;
use crate::auth::storage::load_auth_for_migration;
use crate::auth_accounts::ACCOUNTS_FILE_VERSION;
use crate::auth_accounts::AccountsFile;
use crate::auth_accounts::read_accounts_file_for_migration;

pub(super) const LOGIN_AGGREGATE_SECRET: &str = "LOGIN_CREDENTIALS";
const LOGIN_AGGREGATE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginAggregateV1 {
    pub version: u32,
    pub provenance: AggregateProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_auth: Option<AuthDotJson>,
    pub(crate) accounts: AccountsFile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateProvenance {
    pub store_mode: AuthCredentialsStoreMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_auth_source: Option<AggregateAuthSource>,
    pub catalog_present: bool,
    pub assembled_from: DocumentOrigin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateAuthSource {
    File,
    Keyring,
}

impl From<AuthStorageSource> for AggregateAuthSource {
    fn from(source: AuthStorageSource) -> Self {
        match source {
            AuthStorageSource::File => Self::File,
            AuthStorageSource::Keyring => Self::Keyring,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentOrigin {
    LegacyMigration,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PreparedMigration {
    Nothing,
    AlreadyEncrypted(LoginAggregateV1),
    Prepared(LoginAggregateV1),
}

/// Prepare an encrypted aggregate while leaving legacy auth sources authoritative.
///
/// This writes the encrypted aggregate plus the local-secrets lock metadata and,
/// when absent, its namespaced keyring encryption key. It never deletes,
/// rewrites, or activates `auth.json`, the keyring auth entry, or
/// `auth_accounts.json`.
pub fn prepare_encrypted_aggregate(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> io::Result<PreparedMigration> {
    if mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(PreparedMigration::Nothing);
    }

    let manager = secrets_manager(codex_home, keyring_store.clone());
    let name = aggregate_secret_name()?;
    let existing = manager
        .get(&SecretScope::Global, &name)
        .map_err(secret_err)?
        .map(|raw| parse_document(&raw))
        .transpose()?;
    let candidate = read_legacy_document(codex_home, mode, keyring_store)?;

    if let Some(existing) = existing {
        let Some(candidate) = candidate else {
            return Err(stale_document_error());
        };
        if existing != candidate {
            return Err(stale_document_error());
        }
        return Ok(PreparedMigration::AlreadyEncrypted(existing));
    }

    let Some(document) = candidate else {
        return Ok(PreparedMigration::Nothing);
    };
    let serialized = serde_json::to_string(&document).map_err(io::Error::other)?;
    let mut raced_existing = None;
    manager
        .mutate(&SecretScope::Global, &name, |current| {
            let Some(current) = current else {
                return Ok(SecretMutation::Set(serialized.clone()));
            };
            let current = parse_document(current)?;
            if current != document {
                return Err(stale_document_error().into());
            }
            raced_existing = Some(current);
            Ok(SecretMutation::Keep)
        })
        .map_err(secret_err)?;
    if let Some(existing) = raced_existing {
        return Ok(PreparedMigration::AlreadyEncrypted(existing));
    }

    let verified = manager
        .get(&SecretScope::Global, &name)
        .map_err(secret_err)?
        .ok_or_else(|| io::Error::other("encrypted login aggregate missing after write"))?;
    let verified = parse_document(&verified)?;
    if verified != document {
        return Err(io::Error::other(
            "encrypted login aggregate read-back verification failed",
        ));
    }

    Ok(PreparedMigration::Prepared(document))
}

fn read_legacy_document(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> io::Result<Option<LoginAggregateV1>> {
    let (active_auth, active_auth_source) =
        load_auth_for_migration(codex_home, mode, keyring_store)?;
    let (accounts, catalog_present) = read_accounts_file_for_migration(codex_home)?;
    if active_auth.is_none() && !catalog_present {
        return Ok(None);
    }

    let document = LoginAggregateV1 {
        version: LOGIN_AGGREGATE_VERSION,
        provenance: AggregateProvenance {
            store_mode: mode,
            active_auth_source: active_auth_source.map(AggregateAuthSource::from),
            catalog_present,
            assembled_from: DocumentOrigin::LegacyMigration,
        },
        active_auth,
        accounts,
    };
    validate_document(&document)?;
    Ok(Some(document))
}

fn secrets_manager(codex_home: &Path, keyring_store: Arc<dyn KeyringStore>) -> SecretsManager {
    SecretsManager::new_with_keyring_store_and_namespace(
        codex_home.to_path_buf(),
        SecretsBackendKind::Local,
        keyring_store,
        LocalSecretsNamespace::CodexAuth,
    )
}

fn aggregate_secret_name() -> io::Result<SecretName> {
    SecretName::new(LOGIN_AGGREGATE_SECRET).map_err(secret_err)
}

fn parse_document(raw: &str) -> io::Result<LoginAggregateV1> {
    let value: Value = serde_json::from_str(raw).map_err(io::Error::other)?;
    if value.pointer("/accounts/version").is_none() {
        return Err(io::Error::other(
            "encrypted login aggregate accounts version is missing",
        ));
    }
    let document: LoginAggregateV1 = serde_json::from_value(value).map_err(io::Error::other)?;
    validate_document(&document)?;
    Ok(document)
}

fn validate_document(document: &LoginAggregateV1) -> io::Result<()> {
    if document.version != LOGIN_AGGREGATE_VERSION {
        return Err(io::Error::other(format!(
            "unsupported encrypted login aggregate version {}",
            document.version
        )));
    }
    if document.accounts.version != ACCOUNTS_FILE_VERSION {
        return Err(io::Error::other(format!(
            "unsupported encrypted login aggregate accounts version {}",
            document.accounts.version
        )));
    }
    if document.provenance.store_mode == AuthCredentialsStoreMode::Ephemeral {
        return Err(io::Error::other(
            "encrypted login aggregate cannot use ephemeral provenance",
        ));
    }
    if document.provenance.active_auth_source.is_some() != document.active_auth.is_some() {
        return Err(io::Error::other(
            "encrypted login aggregate active auth provenance is inconsistent",
        ));
    }
    match (
        document.provenance.store_mode,
        document.provenance.active_auth_source,
    ) {
        (AuthCredentialsStoreMode::File, Some(AggregateAuthSource::Keyring))
        | (AuthCredentialsStoreMode::Keyring, Some(AggregateAuthSource::File)) => {
            return Err(io::Error::other(
                "encrypted login aggregate auth source conflicts with store mode",
            ));
        }
        _ => {}
    }
    if !document.provenance.catalog_present && document.accounts != AccountsFile::default() {
        return Err(io::Error::other(
            "encrypted login aggregate catalog provenance is inconsistent",
        ));
    }
    if document.active_auth.is_none() && !document.provenance.catalog_present {
        return Err(io::Error::other(
            "encrypted login aggregate has no legacy source",
        ));
    }
    validate_active_account(document.active_auth.as_ref(), &document.accounts)
}

fn validate_active_account(
    active_auth: Option<&AuthDotJson>,
    accounts: &AccountsFile,
) -> io::Result<()> {
    let Some(active_account_id) = accounts.active_account_id.as_deref() else {
        return Ok(());
    };
    let active_account = accounts
        .accounts
        .iter()
        .find(|account| account.id == active_account_id)
        .ok_or_else(|| io::Error::other("active auth account is missing from the catalog"))?;
    let Some(active_auth) = active_auth else {
        return Ok(());
    };
    if active_auth
        .auth_mode
        .as_ref()
        .is_some_and(|mode| mode != &active_account.mode)
        || active_auth.openai_api_key != active_account.openai_api_key
        || active_auth.tokens != active_account.tokens
        || active_auth.last_refresh != active_account.last_refresh
    {
        return Err(io::Error::other(
            "active auth does not match the active account catalog entry",
        ));
    }
    Ok(())
}

fn stale_document_error() -> io::Error {
    io::Error::other("encrypted login aggregate does not match current legacy sources")
}

fn secret_err(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
