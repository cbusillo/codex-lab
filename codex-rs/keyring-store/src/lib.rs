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

#[cfg(debug_assertions)]
pub const TEST_KEYRING_DIR_ENV_VAR: &str = "CODEX_APP_SERVER_TEST_KEYRING_DIR";

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
fn set_default_keyring_store_for_tests(keyring_store: Arc<dyn KeyringStore>) -> bool {
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
    use std::fs::OpenOptions;
    #[cfg(debug_assertions)]
    use std::io::Write;
    #[cfg(all(debug_assertions, unix))]
    use std::os::unix::fs::DirBuilderExt;
    #[cfg(all(debug_assertions, unix))]
    use std::os::unix::fs::OpenOptionsExt;
    #[cfg(all(debug_assertions, unix))]
    use std::os::unix::fs::PermissionsExt;
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
    static SHARED_TEST_KEYRING_ROOT: OnceLock<SharedTestKeyringRoot> = OnceLock::new();

    #[cfg(debug_assertions)]
    struct SharedTestKeyringRoot {
        path: PathBuf,
        owned: bool,
    }

    #[cfg(debug_assertions)]
    #[derive(Debug)]
    pub struct HermeticTestKeyringStore {
        root: PathBuf,
        next_temp_file_id: AtomicU64,
    }

    #[cfg(debug_assertions)]
    impl HermeticTestKeyringStore {
        pub fn persisted(root: PathBuf) -> Self {
            Self {
                root,
                next_temp_file_id: AtomicU64::new(0),
            }
        }

        fn entry_path(&self, service: &str, account: &str) -> PathBuf {
            self.root
                .join(encoded_path_component(service))
                .join(encoded_path_component(account))
        }
    }

    #[cfg(debug_assertions)]
    impl KeyringStore for HermeticTestKeyringStore {
        fn load(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<String>, CredentialStoreError> {
            match std::fs::read(self.entry_path(service, account)) {
                Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|error| {
                    CredentialStoreError::new(KeyringError::BadEncoding(error.into_bytes()))
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(file_store_error(error)),
            }
        }

        fn save(
            &self,
            service: &str,
            account: &str,
            value: &str,
        ) -> Result<(), CredentialStoreError> {
            secure_directory(&self.root)?;
            let path = self.entry_path(service, account);
            let parent = path.parent().ok_or_else(|| {
                file_store_error(std::io::Error::other("test keyring path has no parent"))
            })?;
            secure_directory(parent)?;
            let process_id = std::process::id();
            let encoded_account = encoded_path_component(account);
            let (temp_path, mut temp_file) = loop {
                let temp_file_id = self.next_temp_file_id.fetch_add(1, Ordering::Relaxed);
                let temp_path = parent.join(format!(
                    ".{process_id}.{temp_file_id}.{encoded_account}.tmp"
                ));
                let mut open_options = OpenOptions::new();
                open_options.create_new(true).write(true);
                #[cfg(unix)]
                open_options.mode(0o600);
                match open_options.open(&temp_path) {
                    Ok(temp_file) => break (temp_path, temp_file),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(file_store_error(error)),
                }
            };
            temp_file
                .write_all(value.as_bytes())
                .map_err(file_store_error)?;
            temp_file.sync_all().map_err(file_store_error)?;
            drop(temp_file);
            if let Err(error) = replace_file(&temp_path, &path) {
                let _ = std::fs::remove_file(&temp_path);
                return Err(file_store_error(error));
            }
            Ok(())
        }

        fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError> {
            match std::fs::remove_file(self.entry_path(service, account)) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(file_store_error(error)),
            }
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
    fn secure_directory(path: &Path) -> Result<(), CredentialStoreError> {
        #[cfg(unix)]
        {
            let mut dir_builder = std::fs::DirBuilder::new();
            dir_builder.recursive(true).mode(0o700);
            dir_builder.create(path).map_err(file_store_error)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(file_store_error)?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(path).map_err(file_store_error)?;
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
        #[cfg(not(windows))]
        {
            std::fs::rename(source, destination)
        }
        #[cfg(windows)]
        {
            match replace_file_windows(source, destination) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::rename(source, destination)
                }
                Err(error) => Err(error),
            }
        }
    }

    #[cfg(all(debug_assertions, windows))]
    fn replace_file_windows(source: &Path, destination: &Path) -> std::io::Result<()> {
        use std::ffi::c_void;
        use std::os::windows::ffi::OsStrExt;

        const REPLACEFILE_IGNORE_MERGE_ERRORS: u32 = 0x0000_0002;

        unsafe extern "system" {
            fn ReplaceFileW(
                replaced_file_name: *const u16,
                replacement_file_name: *const u16,
                backup_file_name: *const u16,
                replace_flags: u32,
                exclude: *mut c_void,
                reserved: *mut c_void,
            ) -> i32;
        }

        let destination_wide: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let replaced = unsafe {
            ReplaceFileW(
                destination_wide.as_ptr(),
                source_wide.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_IGNORE_MERGE_ERRORS,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if replaced == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn shared_test_keyring_root() -> &'static Path {
        SHARED_TEST_KEYRING_ROOT
            .get_or_init(|| {
                if let Some(path) = std::env::var_os(super::TEST_KEYRING_DIR_ENV_VAR) {
                    let path = PathBuf::from(path);
                    if let Err(error) = secure_directory(&path) {
                        panic!("open shared app-server test keyring root: {error}");
                    }
                    return SharedTestKeyringRoot { path, owned: false };
                }
                let process_id = std::process::id();
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let root = std::env::temp_dir().join(format!(
                    "codex-app-server-test-keyring-{process_id}-{timestamp}"
                ));
                #[cfg(unix)]
                {
                    let mut dir_builder = std::fs::DirBuilder::new();
                    dir_builder.mode(0o700);
                    if let Err(error) = dir_builder.create(&root) {
                        panic!("create shared app-server test keyring root: {error}");
                    }
                }
                #[cfg(not(unix))]
                if let Err(error) = std::fs::create_dir(&root) {
                    panic!("create shared app-server test keyring root: {error}");
                }
                SharedTestKeyringRoot {
                    path: root,
                    owned: true,
                }
            })
            .path
            .as_path()
    }

    #[cfg(debug_assertions)]
    pub fn remove_shared_test_keyring_root() -> std::io::Result<()> {
        if let Some(root) = SHARED_TEST_KEYRING_ROOT.get()
            && root.owned
        {
            std::fs::remove_dir_all(&root.path)?;
        }
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub fn install_persisted_default_test_keyring_store(
        root: &Path,
    ) -> Result<bool, CredentialStoreError> {
        secure_directory(root)?;
        Ok(super::set_default_keyring_store_for_tests(Arc::new(
            HermeticTestKeyringStore::persisted(root.to_path_buf()),
        )))
    }
}
