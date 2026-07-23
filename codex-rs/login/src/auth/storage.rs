use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use tracing::warn;

use super::BedrockApiKeyAuth;
use crate::token_data::TokenData;
use codex_agent_identity::AgentIdentityJwtClaims;
use codex_agent_identity::decode_agent_identity_jwt;
use codex_config::types::AuthCredentialsStoreMode;
pub use codex_config::types::AuthKeyringBackendKind;
use codex_keyring_store::DefaultKeyringStore;
use codex_keyring_store::KeyringStore;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::AuthMode;
use codex_secrets::LocalSecretsNamespace;
use codex_secrets::SecretName;
use codex_secrets::SecretScope;
use codex_secrets::SecretsBackendKind;
use codex_secrets::SecretsManager;
use once_cell::sync::Lazy;

use super::atomic_file::write_auth_file_atomically;
use super::encrypted_aggregate::PreparedMigration;
use super::encrypted_aggregate::activate_encrypted_aggregate_with_keyring_backend;
use super::encrypted_aggregate::is_encrypted_aggregate_enabled;
use super::encrypted_aggregate::validate_encrypted_aggregate_for_read;
use super::encrypted_aggregate::with_conditionally_invalidated_encrypted_aggregate;
use super::encrypted_aggregate::with_invalidated_encrypted_aggregate;

const AUTH_STORAGE_LOCK_FILE_NAME: &str = ".auth-storage.lock";
static AUTH_FILE_WRITE_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Expected structure for $CODEX_HOME/auth.json.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct AuthDotJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<AuthMode>,

    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenData>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<AgentIdentityStorage>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personal_access_token: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bedrock_api_key: Option<BedrockApiKeyAuth>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum AgentIdentityStorage {
    Jwt(String),
    Record(AgentIdentityAuthRecord),
}

impl AgentIdentityStorage {
    pub fn has_auth_material(&self) -> bool {
        match self {
            Self::Jwt(jwt) => !jwt.trim().is_empty(),
            Self::Record(record) => {
                !record.agent_runtime_id.trim().is_empty()
                    && !record.agent_private_key.trim().is_empty()
            }
        }
    }

    pub(crate) fn as_record(&self) -> Option<&AgentIdentityAuthRecord> {
        match self {
            Self::Jwt(_) => None,
            Self::Record(record) => Some(record),
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct AgentIdentityAuthRecord {
    pub agent_runtime_id: String,
    pub agent_private_key: String,
    pub account_id: String,
    pub chatgpt_user_id: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_string",
        serialize_with = "serialize_optional_string_as_empty"
    )]
    pub email: Option<String>,
    pub plan_type: AccountPlanType,
    pub chatgpt_account_is_fedramp: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

fn deserialize_optional_non_empty_string<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|value| value.filter(|value| !value.is_empty()))
}

fn serialize_optional_string_as_empty<S>(
    value: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    value.as_deref().unwrap_or_default().serialize(serializer)
}

impl AgentIdentityAuthRecord {
    pub(crate) fn from_agent_identity_jwt(jwt: &str) -> std::io::Result<Self> {
        let claims =
            decode_agent_identity_jwt(jwt, /*jwks*/ None).map_err(std::io::Error::other)?;

        Ok(claims.into())
    }
}

impl From<AgentIdentityJwtClaims> for AgentIdentityAuthRecord {
    fn from(claims: AgentIdentityJwtClaims) -> Self {
        Self {
            agent_runtime_id: claims.agent_runtime_id,
            agent_private_key: claims.agent_private_key,
            account_id: claims.account_id,
            chatgpt_user_id: claims.chatgpt_user_id,
            email: claims.email,
            plan_type: claims.plan_type.into(),
            chatgpt_account_is_fedramp: claims.chatgpt_account_is_fedramp,
            task_id: None,
        }
    }
}

pub(super) fn get_auth_file(codex_home: &Path) -> PathBuf {
    codex_home.join("auth.json")
}

pub(super) fn delete_file_if_exists(codex_home: &Path) -> std::io::Result<bool> {
    let auth_file = get_auth_file(codex_home);
    match std::fs::remove_file(&auth_file) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

pub(super) trait AuthStorageBackend: Debug + Send + Sync {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>>;
    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()>;
    fn delete(&self) -> std::io::Result<bool>;

    fn compare_and_swap(
        &self,
        expected: &AuthDotJson,
        replacement: &AuthDotJson,
    ) -> std::io::Result<bool> {
        if self.load()?.as_ref() != Some(expected) {
            return Ok(false);
        }
        self.save(replacement)?;
        Ok(true)
    }
}

#[derive(Clone, Debug)]
struct AggregateAwareAuthStorage {
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
    keyring_backend_kind: AuthKeyringBackendKind,
    legacy: Arc<dyn AuthStorageBackend>,
}

impl AuthStorageBackend for AggregateAwareAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        if self
            .codex_home
            .metadata()
            .is_ok_and(|metadata| metadata.permissions().readonly())
        {
            validate_encrypted_aggregate_for_read(
                &self.codex_home,
                self.mode,
                self.keyring_store.clone(),
            )?;
            return self.legacy.load();
        }
        match activate_encrypted_aggregate_with_keyring_backend(
            &self.codex_home,
            self.mode,
            self.keyring_store.clone(),
            self.keyring_backend_kind,
        )? {
            PreparedMigration::AlreadyEncrypted(document)
            | PreparedMigration::Prepared(document) => Ok(document.active_auth),
            PreparedMigration::Deferred | PreparedMigration::Nothing => self.legacy.load(),
        }
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        with_invalidated_encrypted_aggregate(
            &self.codex_home,
            self.mode,
            self.keyring_store.clone(),
            || self.legacy.save(auth),
        )
    }

    fn delete(&self) -> std::io::Result<bool> {
        with_invalidated_encrypted_aggregate(
            &self.codex_home,
            self.mode,
            self.keyring_store.clone(),
            || self.legacy.delete(),
        )
    }

    fn compare_and_swap(
        &self,
        expected: &AuthDotJson,
        replacement: &AuthDotJson,
    ) -> std::io::Result<bool> {
        with_conditionally_invalidated_encrypted_aggregate(
            &self.codex_home,
            self.mode,
            self.keyring_store.clone(),
            || self.legacy.compare_and_swap(expected, replacement),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthStorageSource {
    File,
    Keyring,
}

#[derive(Clone, Debug)]
pub(super) struct FileAuthStorage {
    codex_home: PathBuf,
}

struct AuthFileWriteGuard {
    _process_guard: MutexGuard<'static, ()>,
    lock_file: File,
}

impl Drop for AuthFileWriteGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.lock_file);
    }
}

impl FileAuthStorage {
    pub(super) fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    /// Attempt to read and parse the `auth.json` file in the given `CODEX_HOME` directory.
    /// Returns the full AuthDotJson structure.
    pub(super) fn try_read_auth_json(&self, auth_file: &Path) -> std::io::Result<AuthDotJson> {
        let mut file = File::open(auth_file)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let auth_dot_json: AuthDotJson = serde_json::from_str(&contents)?;

        Ok(auth_dot_json)
    }

    fn acquire_write_guard(&self) -> std::io::Result<AuthFileWriteGuard> {
        let process_guard = AUTH_FILE_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::fs::create_dir_all(&self.codex_home)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let lock_file = options.open(self.codex_home.join(AUTH_STORAGE_LOCK_FILE_NAME))?;
        fs2::FileExt::lock_exclusive(&lock_file)?;
        Ok(AuthFileWriteGuard {
            _process_guard: process_guard,
            lock_file,
        })
    }

    fn load_unlocked(&self) -> std::io::Result<Option<AuthDotJson>> {
        let auth_file = get_auth_file(&self.codex_home);
        match self.try_read_auth_json(&auth_file) {
            Ok(auth) => Ok(Some(auth)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn save_unlocked(&self, auth_dot_json: &AuthDotJson) -> std::io::Result<()> {
        let auth_file = get_auth_file(&self.codex_home);
        let json_data = serde_json::to_string_pretty(auth_dot_json)?;
        write_auth_file_atomically(&auth_file, json_data.as_bytes())
    }
}

impl AuthStorageBackend for FileAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        self.load_unlocked()
    }

    fn save(&self, auth_dot_json: &AuthDotJson) -> std::io::Result<()> {
        let _guard = self.acquire_write_guard()?;
        self.save_unlocked(auth_dot_json)
    }

    fn delete(&self) -> std::io::Result<bool> {
        let _guard = self.acquire_write_guard()?;
        delete_file_if_exists(&self.codex_home)
    }

    fn compare_and_swap(
        &self,
        expected: &AuthDotJson,
        replacement: &AuthDotJson,
    ) -> std::io::Result<bool> {
        let _guard = self.acquire_write_guard()?;
        if self.load_unlocked()?.as_ref() != Some(expected) {
            return Ok(false);
        }
        self.save_unlocked(replacement)?;
        Ok(true)
    }
}

static CODEX_AUTH_SECRET_NAME: Lazy<SecretName> =
    Lazy::new(|| match SecretName::new("CODEX_AUTH") {
        Ok(name) => name,
        Err(err) => unreachable!("CODEX_AUTH should be a valid secret name: {err}"),
    });
const KEYRING_SERVICE: &str = "Codex Auth";

// turns codex_home path into a stable, short key string
fn compute_store_key(codex_home: &Path) -> std::io::Result<String> {
    let canonical = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf());
    let path_str = canonical.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let truncated = hex.get(..16).unwrap_or(&hex);
    Ok(format!("cli|{truncated}"))
}

#[derive(Clone, Debug)]
struct DirectKeyringAuthStorage {
    codex_home: PathBuf,
    keyring_store: Arc<dyn KeyringStore>,
}

impl DirectKeyringAuthStorage {
    fn new(codex_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        Self {
            codex_home,
            keyring_store,
        }
    }

    fn load_from_keyring(&self, key: &str) -> std::io::Result<Option<AuthDotJson>> {
        match self.keyring_store.load(KEYRING_SERVICE, key) {
            Ok(Some(serialized)) => serde_json::from_str(&serialized).map(Some).map_err(|err| {
                std::io::Error::other(format!(
                    "failed to deserialize CLI auth from keyring: {err}"
                ))
            }),
            Ok(None) => Ok(None),
            Err(error) => Err(std::io::Error::other(format!(
                "failed to load CLI auth from keyring: {}",
                error.message()
            ))),
        }
    }

    fn save_to_keyring(&self, key: &str, value: &str) -> std::io::Result<()> {
        match self.keyring_store.save(KEYRING_SERVICE, key, value) {
            Ok(()) => Ok(()),
            Err(error) => {
                let message = format!(
                    "failed to write OAuth tokens to keyring: {}",
                    error.message()
                );
                warn!("{message}");
                Err(std::io::Error::other(message))
            }
        }
    }
}

impl AuthStorageBackend for DirectKeyringAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let key = compute_store_key(&self.codex_home)?;
        self.load_from_keyring(&key)
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let key = compute_store_key(&self.codex_home)?;
        // Simpler error mapping per style: prefer method reference over closure
        let serialized = serde_json::to_string(auth).map_err(std::io::Error::other)?;
        self.save_to_keyring(&key, &serialized)?;
        if let Err(err) = delete_file_if_exists(&self.codex_home) {
            warn!("failed to remove CLI auth fallback file: {err}");
        }
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        let key = compute_store_key(&self.codex_home)?;
        let keyring_removed = self
            .keyring_store
            .delete(KEYRING_SERVICE, &key)
            .map_err(|err| {
                std::io::Error::other(format!("failed to delete auth from keyring: {err}"))
            })?;
        let file_removed = delete_file_if_exists(&self.codex_home)?;
        Ok(keyring_removed || file_removed)
    }
}

#[derive(Clone)]
struct SecretsKeyringAuthStorage {
    codex_home: PathBuf,
    direct_storage: DirectKeyringAuthStorage,
    secrets_manager: SecretsManager,
}

impl Debug for SecretsKeyringAuthStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsKeyringAuthStorage")
            .field("codex_home", &self.codex_home)
            .finish_non_exhaustive()
    }
}

impl SecretsKeyringAuthStorage {
    fn new(codex_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        let direct_storage =
            DirectKeyringAuthStorage::new(codex_home.clone(), Arc::clone(&keyring_store));
        let secrets_manager = SecretsManager::new_with_keyring_store_and_namespace(
            codex_home.clone(),
            SecretsBackendKind::Local,
            keyring_store,
            LocalSecretsNamespace::CodexAuth,
        );
        Self {
            codex_home,
            direct_storage,
            secrets_manager,
        }
    }
}

impl AuthStorageBackend for SecretsKeyringAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        match self
            .secrets_manager
            .get(&SecretScope::Global, &CODEX_AUTH_SECRET_NAME)
            .map_err(|err| {
                std::io::Error::other(format!(
                    "failed to load CLI auth from encrypted auth storage: {err}"
                ))
            })? {
            Some(serialized) => serde_json::from_str(&serialized).map(Some).map_err(|err| {
                std::io::Error::other(format!(
                    "failed to deserialize CLI auth from encrypted auth storage: {err}"
                ))
            }),
            None => Ok(None),
        }
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let serialized = serde_json::to_string(auth).map_err(std::io::Error::other)?;
        self.secrets_manager
            .set(&SecretScope::Global, &CODEX_AUTH_SECRET_NAME, &serialized)
            .map_err(|err| {
                let message =
                    format!("failed to write OAuth tokens to encrypted auth storage: {err}");
                warn!("{message}");
                std::io::Error::other(message)
            })?;
        if let Err(err) = delete_file_if_exists(&self.codex_home) {
            warn!("failed to remove CLI auth fallback file: {err}");
        }
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        let keyring_removed = self
            .secrets_manager
            .delete(&SecretScope::Global, &CODEX_AUTH_SECRET_NAME)
            .map_err(|err| {
                std::io::Error::other(format!(
                    "failed to delete auth from encrypted auth storage: {err}"
                ))
            })?;
        let file_removed = delete_file_if_exists(&self.codex_home)?;
        let direct_removed = self.direct_storage.delete()?;
        Ok(keyring_removed || file_removed || direct_removed)
    }
}

#[derive(Clone, Debug)]
struct AutoAuthStorage {
    keyring_storage: Arc<dyn AuthStorageBackend>,
    file_storage: Arc<FileAuthStorage>,
}

impl AutoAuthStorage {
    fn new(
        codex_home: PathBuf,
        keyring_store: Arc<dyn KeyringStore>,
        keyring_backend_kind: AuthKeyringBackendKind,
    ) -> Self {
        Self {
            keyring_storage: create_keyring_auth_storage(
                codex_home.clone(),
                keyring_store,
                keyring_backend_kind,
            ),
            file_storage: Arc::new(FileAuthStorage::new(codex_home)),
        }
    }
}

impl AuthStorageBackend for AutoAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        match self.keyring_storage.load() {
            Ok(Some(auth)) => Ok(Some(auth)),
            Ok(None) => self.file_storage.load(),
            Err(err) => {
                warn!("failed to load CLI auth from keyring, falling back to file storage: {err}");
                self.file_storage.load()
            }
        }
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        match self.keyring_storage.save(auth) {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!("failed to save auth to keyring, falling back to file storage: {err}");
                self.file_storage.save(auth)
            }
        }
    }

    fn delete(&self) -> std::io::Result<bool> {
        // Keyring storage will delete from disk as well
        self.keyring_storage.delete()
    }
}

// A global in-memory store for mapping codex_home -> AuthDotJson.
static EPHEMERAL_AUTH_STORE: Lazy<Mutex<HashMap<String, AuthDotJson>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
struct EphemeralAuthStorage {
    codex_home: PathBuf,
}

impl EphemeralAuthStorage {
    fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    fn with_store<F, T>(&self, action: F) -> std::io::Result<T>
    where
        F: FnOnce(&mut HashMap<String, AuthDotJson>, String) -> std::io::Result<T>,
    {
        let key = compute_store_key(&self.codex_home)?;
        let mut store = EPHEMERAL_AUTH_STORE
            .lock()
            .map_err(|_| std::io::Error::other("failed to lock ephemeral auth storage"))?;
        action(&mut store, key)
    }
}

impl AuthStorageBackend for EphemeralAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        self.with_store(|store, key| Ok(store.get(&key).cloned()))
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        self.with_store(|store, key| {
            store.insert(key, auth.clone());
            Ok(())
        })
    }

    fn delete(&self) -> std::io::Result<bool> {
        self.with_store(|store, key| Ok(store.remove(&key).is_some()))
    }

    fn compare_and_swap(
        &self,
        expected: &AuthDotJson,
        replacement: &AuthDotJson,
    ) -> std::io::Result<bool> {
        self.with_store(|store, key| {
            if store.get(&key) != Some(expected) {
                return Ok(false);
            }
            store.insert(key, replacement.clone());
            Ok(true)
        })
    }
}

pub(super) fn create_auth_storage(
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> Arc<dyn AuthStorageBackend> {
    let keyring_store: Arc<dyn KeyringStore> = Arc::new(DefaultKeyringStore);
    create_auth_storage_with_store(codex_home, mode, keyring_store, keyring_backend_kind)
}

fn create_auth_storage_with_store(
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> Arc<dyn AuthStorageBackend> {
    if !is_encrypted_aggregate_enabled(mode) {
        return create_legacy_auth_storage_with_store(
            codex_home,
            mode,
            keyring_store,
            keyring_backend_kind,
        );
    }

    create_aggregate_aware_auth_storage_with_store(
        codex_home,
        mode,
        keyring_store,
        keyring_backend_kind,
    )
}

fn create_aggregate_aware_auth_storage_with_store(
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> Arc<dyn AuthStorageBackend> {
    let legacy = create_legacy_auth_storage_with_store(
        codex_home.clone(),
        mode,
        Arc::clone(&keyring_store),
        keyring_backend_kind,
    );
    Arc::new(AggregateAwareAuthStorage {
        codex_home,
        mode,
        keyring_store,
        keyring_backend_kind,
        legacy,
    })
}

fn create_legacy_auth_storage_with_store(
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> Arc<dyn AuthStorageBackend> {
    match mode {
        AuthCredentialsStoreMode::File => Arc::new(FileAuthStorage::new(codex_home)),
        AuthCredentialsStoreMode::Keyring => {
            create_keyring_auth_storage(codex_home, keyring_store, keyring_backend_kind)
        }
        AuthCredentialsStoreMode::Auto => Arc::new(AutoAuthStorage::new(
            codex_home,
            keyring_store,
            keyring_backend_kind,
        )),
        AuthCredentialsStoreMode::Ephemeral => Arc::new(EphemeralAuthStorage::new(codex_home)),
    }
}

pub(crate) fn load_auth_for_migration(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<(Option<AuthDotJson>, Option<AuthStorageSource>)> {
    match mode {
        AuthCredentialsStoreMode::File => auth_with_source(
            FileAuthStorage::new(codex_home.to_path_buf()).load(),
            AuthStorageSource::File,
        ),
        AuthCredentialsStoreMode::Keyring => auth_with_source(
            create_keyring_auth_storage(
                codex_home.to_path_buf(),
                keyring_store,
                keyring_backend_kind,
            )
            .load(),
            AuthStorageSource::Keyring,
        ),
        AuthCredentialsStoreMode::Auto => {
            let keyring_storage = create_keyring_auth_storage(
                codex_home.to_path_buf(),
                keyring_store,
                keyring_backend_kind,
            );
            match keyring_storage.load() {
                Ok(Some(auth)) => Ok((Some(auth), Some(AuthStorageSource::Keyring))),
                Ok(None) => auth_with_source(
                    FileAuthStorage::new(codex_home.to_path_buf()).load(),
                    AuthStorageSource::File,
                ),
                Err(err) => Err(std::io::Error::other(format!(
                    "failed to load CLI auth from keyring for encrypted migration: {err}"
                ))),
            }
        }
        AuthCredentialsStoreMode::Ephemeral => Ok((None, None)),
    }
}

fn auth_with_source(
    auth: std::io::Result<Option<AuthDotJson>>,
    source: AuthStorageSource,
) -> std::io::Result<(Option<AuthDotJson>, Option<AuthStorageSource>)> {
    let auth = auth?;
    let resolved_source = auth.as_ref().map(|_| source);
    Ok((auth, resolved_source))
}

#[cfg(test)]
pub(crate) fn load_auth_with_keyring_store(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> std::io::Result<Option<AuthDotJson>> {
    create_legacy_auth_storage_with_store(
        codex_home.to_path_buf(),
        mode,
        keyring_store,
        AuthKeyringBackendKind::Direct,
    )
    .load()
}

#[cfg(test)]
pub(crate) fn load_activated_auth_with_keyring_store(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> std::io::Result<Option<AuthDotJson>> {
    create_aggregate_aware_auth_storage_with_store(
        codex_home.to_path_buf(),
        mode,
        keyring_store,
        AuthKeyringBackendKind::Direct,
    )
    .load()
}

#[cfg(test)]
pub(crate) fn auth_keyring_account_for_tests(codex_home: &Path) -> std::io::Result<String> {
    compute_store_key(codex_home)
}

#[cfg(test)]
pub(crate) fn save_auth_with_keyring_store(
    codex_home: &Path,
    auth: &AuthDotJson,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> std::io::Result<()> {
    create_legacy_auth_storage_with_store(
        codex_home.to_path_buf(),
        mode,
        keyring_store,
        AuthKeyringBackendKind::Direct,
    )
    .save(auth)
}

#[cfg(test)]
pub(crate) fn save_activated_auth_with_keyring_store(
    codex_home: &Path,
    auth: &AuthDotJson,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> std::io::Result<()> {
    create_aggregate_aware_auth_storage_with_store(
        codex_home.to_path_buf(),
        mode,
        keyring_store,
        AuthKeyringBackendKind::Direct,
    )
    .save(auth)
}

#[cfg(test)]
pub(crate) fn delete_activated_auth_with_keyring_store(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
    keyring_store: Arc<dyn KeyringStore>,
) -> std::io::Result<bool> {
    create_aggregate_aware_auth_storage_with_store(
        codex_home.to_path_buf(),
        mode,
        keyring_store,
        AuthKeyringBackendKind::Direct,
    )
    .delete()
}

fn create_keyring_auth_storage(
    codex_home: PathBuf,
    keyring_store: Arc<dyn KeyringStore>,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> Arc<dyn AuthStorageBackend> {
    match keyring_backend_kind {
        AuthKeyringBackendKind::Direct => {
            Arc::new(DirectKeyringAuthStorage::new(codex_home, keyring_store))
        }
        AuthKeyringBackendKind::Secrets => {
            Arc::new(SecretsKeyringAuthStorage::new(codex_home, keyring_store))
        }
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
