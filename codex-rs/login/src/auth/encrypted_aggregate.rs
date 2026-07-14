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
pub(crate) struct LoginAggregateV1 {
    pub(crate) version: u32,
    pub(crate) provenance: AggregateProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_auth: Option<AuthDotJson>,
    pub(crate) accounts: AccountsFile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AggregateProvenance {
    pub(crate) store_mode: AuthCredentialsStoreMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_auth_source: Option<AggregateAuthSource>,
    pub(crate) catalog_present: bool,
    pub(crate) assembled_from: DocumentOrigin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AggregateAuthSource {
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
pub(crate) enum DocumentOrigin {
    LegacyMigration,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PreparedMigration {
    Nothing,
    Deferred,
    AlreadyEncrypted(LoginAggregateV1),
    Prepared(LoginAggregateV1),
}

/// Activate the verified encrypted shadow when the current legacy sources form
/// a consistent aggregate.
///
/// Activation and trusted legacy mutations share the same secrets lock. A
/// pre-existing aggregate remains strict, while a first activation is deferred
/// when the legacy sources cannot yet form a consistent snapshot.
pub(crate) fn activate_encrypted_aggregate(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> io::Result<PreparedMigration> {
    if mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(PreparedMigration::Nothing);
    }

    let manager = secrets_manager(codex_home, keyring_store.clone());
    let name = aggregate_secret_name()?;
    let mut activation = None;
    let mut initial_activation = false;
    let mutation_result = manager.mutate(&SecretScope::Global, &name, |current| {
        let mutation = if let Some(current) = current {
            let existing = parse_document(current)?;
            let candidate = match read_legacy_document(codex_home, mode, keyring_store.clone()) {
                Ok(Some(candidate)) => candidate,
                Ok(None) => return Err(stale_document_error().into()),
                Err(_) => {
                    activation = Some(PreparedMigration::Deferred);
                    return Ok(SecretMutation::Keep);
                }
            };
            if existing != candidate {
                return Err(stale_document_error().into());
            }
            activation = Some(PreparedMigration::AlreadyEncrypted(existing));
            SecretMutation::Keep
        } else {
            initial_activation = true;
            let candidate = match assemble_legacy_document(codex_home, mode, keyring_store.clone())
            {
                Ok(candidate) => candidate,
                Err(_) => {
                    activation = Some(PreparedMigration::Deferred);
                    return Ok(SecretMutation::Keep);
                }
            };
            let Some(document) = candidate else {
                activation = Some(PreparedMigration::Nothing);
                return Ok(SecretMutation::Keep);
            };
            if validate_active_account(&document).is_err() {
                activation = Some(PreparedMigration::Deferred);
                return Ok(SecretMutation::Keep);
            }
            let serialized = serde_json::to_string(&document).map_err(io::Error::other)?;
            activation = Some(PreparedMigration::Prepared(document));
            SecretMutation::Set(serialized)
        };
        Ok(mutation)
    });
    if let Err(error) = mutation_result {
        if initial_activation {
            return Ok(PreparedMigration::Deferred);
        }
        return Err(secret_err(error));
    }
    activation.ok_or_else(|| io::Error::other("encrypted login activation did not run"))
}

/// Run a trusted legacy mutation while invalidating any verified encrypted
/// aggregate under the same secrets lock.
///
/// Legacy auth sources remain authoritative during this activation stage. A
/// corrupt encrypted aggregate blocks the mutation instead of being deleted,
/// while a valid aggregate is removed only after the legacy mutation succeeds.
pub(crate) fn with_invalidated_encrypted_aggregate<T>(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
    mutation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    if mode == AuthCredentialsStoreMode::Ephemeral {
        return mutation();
    }

    let manager = secrets_manager(codex_home, keyring_store);
    let name = aggregate_secret_name()?;
    let mut mutation = Some(mutation);
    let mut result = None;
    manager
        .mutate(&SecretScope::Global, &name, |current| {
            if let Some(current) = current {
                parse_document(current)?;
            }
            let mutation = mutation
                .take()
                .ok_or_else(|| io::Error::other("legacy login mutation ran more than once"))?;
            result = Some(mutation()?);
            Ok(if current.is_some() {
                SecretMutation::Delete
            } else {
                SecretMutation::Keep
            })
        })
        .map_err(secret_err)?;
    result.ok_or_else(|| io::Error::other("legacy login mutation did not run"))
}

fn read_legacy_document(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> io::Result<Option<LoginAggregateV1>> {
    let document = assemble_legacy_document(codex_home, mode, keyring_store)?;
    if let Some(document) = document.as_ref() {
        validate_active_account(document)?;
    }
    Ok(document)
}

fn assemble_legacy_document(
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
    validate_document_envelope(&document)?;
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
    validate_document_envelope(document)?;
    validate_active_account(document)
}

fn validate_document_envelope(document: &LoginAggregateV1) -> io::Result<()> {
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
    Ok(())
}

fn validate_active_account(document: &LoginAggregateV1) -> io::Result<()> {
    let accounts = &document.accounts;
    let Some(active_account_id) = accounts.active_account_id.as_deref() else {
        return Ok(());
    };
    if document.active_auth.as_ref().is_some_and(|active_auth| {
        document.provenance.store_mode != AuthCredentialsStoreMode::File
            || active_auth.agent_identity.is_some()
            || active_auth.personal_access_token.is_some()
    }) {
        return Ok(());
    }
    let active_account = accounts
        .accounts
        .iter()
        .find(|account| account.id == active_account_id)
        .ok_or_else(|| io::Error::other("active auth account is missing from the catalog"))?;
    let Some(active_auth) = document.active_auth.as_ref() else {
        return Ok(());
    };
    if active_auth
        .auth_mode
        .as_ref()
        .is_some_and(|mode| mode != &active_account.mode)
        || active_auth.openai_api_key != active_account.openai_api_key
        || active_auth.tokens != active_account.tokens
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
