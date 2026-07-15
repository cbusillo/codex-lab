use keyring::Entry;
use keyring::Error as KeyringError;
use std::error::Error;
use std::fmt;
use std::fmt::Debug;
#[cfg(debug_assertions)]
use std::sync::Arc;
#[cfg(debug_assertions)]
use std::sync::OnceLock;
use tracing::trace;

#[derive(Debug)]
pub enum CredentialStoreError {
    Other(KeyringError),
}

impl CredentialStoreError {
    pub fn new(error: KeyringError) -> Self {
        Self::Other(error)
    }

    pub fn message(&self) -> String {
        match self {
            Self::Other(error) => error.to_string(),
        }
    }

    pub fn into_error(self) -> KeyringError {
        match self {
            Self::Other(error) => error,
        }
    }
}

impl fmt::Display for CredentialStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Other(error) => write!(f, "{error}"),
        }
    }
}

impl Error for CredentialStoreError {}

/// Shared credential store abstraction for keyring-backed implementations.
pub trait KeyringStore: Debug + Send + Sync {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, CredentialStoreError>;
    fn save(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialStoreError>;
    fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError>;
}

#[cfg(debug_assertions)]
static DEFAULT_KEYRING_STORE_OVERRIDE: OnceLock<Arc<dyn KeyringStore>> = OnceLock::new();

#[cfg(debug_assertions)]
pub fn set_default_keyring_store_for_tests(keyring_store: Arc<dyn KeyringStore>) -> bool {
    DEFAULT_KEYRING_STORE_OVERRIDE.set(keyring_store).is_ok()
}

#[derive(Debug, Clone, Copy)]
pub struct DefaultKeyringStore;

impl KeyringStore for DefaultKeyringStore {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, CredentialStoreError> {
        #[cfg(debug_assertions)]
        if let Some(keyring_store) = DEFAULT_KEYRING_STORE_OVERRIDE.get() {
            return keyring_store.load(service, account);
        }
        trace!("keyring.load start, service={service}, account={account}");
        let entry = Entry::new(service, account).map_err(CredentialStoreError::new)?;
        match entry.get_password() {
            Ok(password) => {
                trace!("keyring.load success, service={service}, account={account}");
                Ok(Some(password))
            }
            Err(keyring::Error::NoEntry) => {
                trace!("keyring.load no entry, service={service}, account={account}");
                Ok(None)
            }
            Err(error) => {
                trace!("keyring.load error, service={service}, account={account}, error={error}");
                Err(CredentialStoreError::new(error))
            }
        }
    }

    fn save(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialStoreError> {
        #[cfg(debug_assertions)]
        if let Some(keyring_store) = DEFAULT_KEYRING_STORE_OVERRIDE.get() {
            return keyring_store.save(service, account, value);
        }
        trace!(
            "keyring.save start, service={service}, account={account}, value_len={}",
            value.len()
        );
        let entry = Entry::new(service, account).map_err(CredentialStoreError::new)?;
        match entry.set_password(value) {
            Ok(()) => {
                trace!("keyring.save success, service={service}, account={account}");
                Ok(())
            }
            Err(error) => {
                trace!("keyring.save error, service={service}, account={account}, error={error}");
                Err(CredentialStoreError::new(error))
            }
        }
    }

    fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError> {
        #[cfg(debug_assertions)]
        if let Some(keyring_store) = DEFAULT_KEYRING_STORE_OVERRIDE.get() {
            return keyring_store.delete(service, account);
        }
        trace!("keyring.delete start, service={service}, account={account}");
        let entry = Entry::new(service, account).map_err(CredentialStoreError::new)?;
        match entry.delete_credential() {
            Ok(()) => {
                trace!("keyring.delete success, service={service}, account={account}");
                Ok(true)
            }
            Err(keyring::Error::NoEntry) => {
                trace!("keyring.delete no entry, service={service}, account={account}");
                Ok(false)
            }
            Err(error) => {
                trace!("keyring.delete error, service={service}, account={account}, error={error}");
                Err(CredentialStoreError::new(error))
            }
        }
    }
}

pub mod tests {
    use super::CredentialStoreError;
    use super::KeyringStore;
    use keyring::Error as KeyringError;
    use keyring::credential::CredentialApi as _;
    use keyring::mock::MockCredential;
    use std::collections::HashMap;
    #[cfg(debug_assertions)]
    use std::path::Path;
    #[cfg(debug_assertions)]
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex;
    #[cfg(debug_assertions)]
    use std::sync::OnceLock;
    use std::sync::PoisonError;
    #[cfg(debug_assertions)]
    use std::sync::atomic::AtomicU64;
    #[cfg(debug_assertions)]
    use std::sync::atomic::Ordering;

    #[derive(Default, Clone, Debug)]
    pub struct MockKeyringStore {
        credentials: Arc<Mutex<HashMap<String, Arc<MockCredential>>>>,
    }

    impl MockKeyringStore {
        pub fn credential(&self, account: &str) -> Arc<MockCredential> {
            let mut guard = self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            guard
                .entry(account.to_string())
                .or_insert_with(|| Arc::new(MockCredential::default()))
                .clone()
        }

        pub fn saved_value(&self, account: &str) -> Option<String> {
            let credential = {
                let guard = self
                    .credentials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.get(account).cloned()
            }?;
            credential.get_password().ok()
        }

        pub fn set_error(&self, account: &str, error: KeyringError) {
            let credential = self.credential(account);
            credential.set_error(error);
        }

        pub fn contains(&self, account: &str) -> bool {
            let guard = self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            guard.contains_key(account)
        }
    }

    impl KeyringStore for MockKeyringStore {
        fn load(
            &self,
            _service: &str,
            account: &str,
        ) -> Result<Option<String>, CredentialStoreError> {
            let credential = {
                let guard = self
                    .credentials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.get(account).cloned()
            };

            let Some(credential) = credential else {
                return Ok(None);
            };

            match credential.get_password() {
                Ok(password) => Ok(Some(password)),
                Err(KeyringError::NoEntry) => Ok(None),
                Err(error) => Err(CredentialStoreError::new(error)),
            }
        }

        fn save(
            &self,
            _service: &str,
            account: &str,
            value: &str,
        ) -> Result<(), CredentialStoreError> {
            let credential = self.credential(account);
            credential
                .set_password(value)
                .map_err(CredentialStoreError::new)
        }

        fn delete(&self, _service: &str, account: &str) -> Result<bool, CredentialStoreError> {
            let credential = {
                let guard = self
                    .credentials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.get(account).cloned()
            };

            let Some(credential) = credential else {
                return Ok(false);
            };

            let removed = match credential.delete_credential() {
                Ok(()) => Ok(true),
                Err(KeyringError::NoEntry) => Ok(false),
                Err(error) => Err(CredentialStoreError::new(error)),
            }?;

            let mut guard = self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            guard.remove(account);
            Ok(removed)
        }
    }

    #[cfg(debug_assertions)]
    const TEST_SECRETS_PASSPHRASE: &str = "codex-app-server-integration-test-passphrase";
    #[cfg(debug_assertions)]
    static SHARED_TEST_KEYRING_ROOT: OnceLock<PathBuf> = OnceLock::new();

    #[cfg(debug_assertions)]
    #[derive(Debug)]
    pub struct HermeticTestKeyringStore {
        fallback: MockKeyringStore,
        persisted_root: Option<PathBuf>,
        next_temp_file_id: AtomicU64,
    }

    #[cfg(debug_assertions)]
    impl Default for HermeticTestKeyringStore {
        fn default() -> Self {
            Self {
                fallback: MockKeyringStore::default(),
                persisted_root: None,
                next_temp_file_id: AtomicU64::new(0),
            }
        }
    }

    #[cfg(debug_assertions)]
    impl HermeticTestKeyringStore {
        pub fn persisted(root: PathBuf) -> Self {
            Self {
                persisted_root: Some(root),
                ..Self::default()
            }
        }

        fn entry_path(&self, service: &str, account: &str) -> Option<PathBuf> {
            self.persisted_root.as_ref().map(|root| {
                root.join(encoded_path_component(service))
                    .join(encoded_path_component(account))
            })
        }

        fn load_value(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<String>, CredentialStoreError> {
            let Some(path) = self.entry_path(service, account) else {
                return self.fallback.load(service, account);
            };
            match std::fs::read(path) {
                Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|error| {
                    CredentialStoreError::new(KeyringError::BadEncoding(error.into_bytes()))
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(file_store_error(error)),
            }
        }

        fn save_value(
            &self,
            service: &str,
            account: &str,
            value: &str,
        ) -> Result<(), CredentialStoreError> {
            let Some(path) = self.entry_path(service, account) else {
                return self.fallback.save(service, account, value);
            };
            let parent = path.parent().ok_or_else(|| {
                file_store_error(std::io::Error::other("test keyring path has no parent"))
            })?;
            std::fs::create_dir_all(parent).map_err(file_store_error)?;
            let temp_file_id = self.next_temp_file_id.fetch_add(1, Ordering::Relaxed);
            let process_id = std::process::id();
            let encoded_account = encoded_path_component(account);
            let temp_path = parent.join(format!(
                ".{process_id}.{temp_file_id}.{encoded_account}.tmp"
            ));
            std::fs::write(&temp_path, value).map_err(file_store_error)?;
            #[cfg(windows)]
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                let _ = std::fs::remove_file(&temp_path);
                return Err(file_store_error(error));
            }
            if let Err(error) = std::fs::rename(&temp_path, &path) {
                let _ = std::fs::remove_file(&temp_path);
                return Err(file_store_error(error));
            }
            Ok(())
        }

        fn delete_value(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError> {
            let Some(path) = self.entry_path(service, account) else {
                return self.fallback.delete(service, account);
            };
            match std::fs::remove_file(path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(file_store_error(error)),
            }
        }
    }

    #[cfg(debug_assertions)]
    impl KeyringStore for HermeticTestKeyringStore {
        fn load(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<String>, CredentialStoreError> {
            let value = self.load_value(service, account)?;
            if service == "codex" && account.starts_with("secrets|") && value.is_none() {
                self.save_value(service, account, TEST_SECRETS_PASSPHRASE)?;
                return Ok(Some(TEST_SECRETS_PASSPHRASE.to_string()));
            }
            Ok(value)
        }

        fn save(
            &self,
            service: &str,
            account: &str,
            value: &str,
        ) -> Result<(), CredentialStoreError> {
            self.save_value(service, account, value)
        }

        fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError> {
            self.delete_value(service, account)
        }
    }

    #[cfg(debug_assertions)]
    fn encoded_path_component(value: &str) -> String {
        const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(value.len() * 2);
        for byte in value.as_bytes() {
            encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
        }
        encoded
    }

    #[cfg(debug_assertions)]
    fn file_store_error(error: std::io::Error) -> CredentialStoreError {
        CredentialStoreError::new(KeyringError::PlatformFailure(Box::new(error)))
    }

    #[cfg(debug_assertions)]
    pub fn shared_test_keyring_root() -> &'static Path {
        SHARED_TEST_KEYRING_ROOT
            .get_or_init(|| {
                let process_id = std::process::id();
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                std::env::temp_dir().join(format!(
                    "codex-app-server-test-keyring-{process_id}-{timestamp}"
                ))
            })
            .as_path()
    }

    #[cfg(debug_assertions)]
    pub fn install_persisted_default_test_keyring_store(root: &Path) -> bool {
        super::set_default_keyring_store_for_tests(Arc::new(HermeticTestKeyringStore::persisted(
            root.to_path_buf(),
        )))
    }
}
