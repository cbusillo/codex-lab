use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use codex_keyring_store::DefaultKeyringStore;
use codex_keyring_store::KeyringStore;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::LocalSecretsNamespace;
use crate::SecretName;
use crate::SecretScope;
use crate::SecretsBackendKind;
use crate::SecretsManager;

/// Defines a typed payload stored as one versioned encrypted document.
///
/// Implementations provide a stable document name and schema version. Readers
/// reject any stored version that differs from the implementation's version.
pub trait EncryptedDocument: Serialize + DeserializeOwned {
    const NAME: &'static str;
    const SCHEMA_VERSION: u32;
}

/// Stores typed encrypted documents in one isolated local-secrets namespace.
#[derive(Clone)]
pub struct EncryptedDocumentStore {
    manager: SecretsManager,
}

impl fmt::Debug for EncryptedDocumentStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncryptedDocumentStore")
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize, Serialize)]
struct StoredDocumentEnvelope<T> {
    schema_version: u32,
    payload: T,
}

impl EncryptedDocumentStore {
    /// Creates a document store backed by the system keyring.
    pub fn new(codex_home: PathBuf, namespace: LocalSecretsNamespace) -> Self {
        let keyring_store: Arc<dyn KeyringStore> = Arc::new(DefaultKeyringStore);
        Self::new_with_keyring_store(codex_home, namespace, keyring_store)
    }

    /// Creates a document store with an injected keyring implementation.
    pub fn new_with_keyring_store(
        codex_home: PathBuf,
        namespace: LocalSecretsNamespace,
        keyring_store: Arc<dyn KeyringStore>,
    ) -> Self {
        let manager = SecretsManager::new_with_keyring_store_and_namespace(
            codex_home,
            SecretsBackendKind::Local,
            keyring_store,
            namespace,
        );
        Self { manager }
    }

    /// Loads and validates a typed document, returning `None` when it is absent.
    pub fn load<T: EncryptedDocument>(&self) -> Result<Option<T>> {
        let name = document_name::<T>()?;
        let Some(serialized) = self.manager.get(&SecretScope::Global, &name)? else {
            return Ok(None);
        };
        let envelope: StoredDocumentEnvelope<T> = serde_json::from_str(&serialized)
            .with_context(|| format!("failed to deserialize encrypted document {}", T::NAME))?;
        anyhow::ensure!(
            envelope.schema_version == T::SCHEMA_VERSION,
            "encrypted document {} uses schema version {}, expected {}",
            T::NAME,
            envelope.schema_version,
            T::SCHEMA_VERSION
        );
        Ok(Some(envelope.payload))
    }

    /// Atomically stores a typed document in its versioned envelope.
    pub fn store<T: EncryptedDocument>(&self, document: &T) -> Result<()> {
        let name = document_name::<T>()?;
        let envelope = StoredDocumentEnvelope {
            schema_version: T::SCHEMA_VERSION,
            payload: document,
        };
        let serialized = serde_json::to_string(&envelope)
            .with_context(|| format!("failed to serialize encrypted document {}", T::NAME))?;
        self.manager.set(&SecretScope::Global, &name, &serialized)
    }

    /// Deletes a typed document if it exists.
    pub fn delete<T: EncryptedDocument>(&self) -> Result<bool> {
        let name = document_name::<T>()?;
        self.manager.delete(&SecretScope::Global, &name)
    }
}

fn document_name<T: EncryptedDocument>() -> Result<SecretName> {
    SecretName::new(T::NAME).with_context(|| format!("invalid encrypted document name {}", T::NAME))
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
