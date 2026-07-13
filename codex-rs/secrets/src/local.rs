use std::collections::BTreeMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::atomic::compiler_fence;

use age::decrypt;
use age::encrypt;
use age::scrypt::Identity as ScryptIdentity;
use age::scrypt::Recipient as ScryptRecipient;
use age::secrecy::ExposeSecret;
use age::secrecy::SecretString;
use anyhow::Context;
use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_keyring_store::KeyringStore;
use fs2::FileExt;
use rand::TryRngCore;
use rand::rngs::OsRng;
use serde::Deserialize;
use serde::Serialize;
use tracing::debug;
use tracing::warn;

use super::SecretListEntry;
use super::SecretName;
use super::SecretScope;
use super::SecretsBackend;
use super::atomic_file;
use super::compute_keyring_account_for_namespace;
use super::keyring_service;

#[cfg(windows)]
mod windows;

const SECRETS_VERSION: u8 = 1;
const LOCAL_SECRETS_FILENAME: &str = "local.age";
const CODEX_AUTH_SECRETS_FILENAME: &str = "codex_auth.age";
const MCP_OAUTH_SECRETS_FILENAME: &str = "mcp_oauth.age";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalSecretsNamespace {
    #[default]
    ManagedSecrets,
    CodexAuth,
    McpOAuth,
}

impl LocalSecretsNamespace {
    fn filename(self) -> &'static str {
        match self {
            Self::ManagedSecrets => LOCAL_SECRETS_FILENAME,
            Self::CodexAuth => CODEX_AUTH_SECRETS_FILENAME,
            Self::McpOAuth => MCP_OAUTH_SECRETS_FILENAME,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct SecretsFile {
    version: u8,
    secrets: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
enum LockMode {
    Shared,
    Exclusive,
}

impl SecretsFile {
    fn new_empty() -> Self {
        Self {
            version: SECRETS_VERSION,
            secrets: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LocalSecretsBackend {
    codex_home: PathBuf,
    keyring_store: Arc<dyn KeyringStore>,
    namespace: LocalSecretsNamespace,
}

impl LocalSecretsBackend {
    pub fn new(codex_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        Self::new_with_namespace(
            codex_home,
            keyring_store,
            LocalSecretsNamespace::ManagedSecrets,
        )
    }

    pub fn new_with_namespace(
        codex_home: PathBuf,
        keyring_store: Arc<dyn KeyringStore>,
        namespace: LocalSecretsNamespace,
    ) -> Self {
        Self {
            codex_home,
            keyring_store,
            namespace,
        }
    }

    pub fn set(&self, scope: &SecretScope, name: &SecretName, value: &str) -> Result<()> {
        anyhow::ensure!(!value.is_empty(), "secret value must not be empty");
        let _lock = self.acquire_lock(LockMode::Exclusive)?;
        #[cfg(windows)]
        self.recover_windows_atomic_write()?;
        let canonical_key = scope.canonical_key(name);
        let mut file = self.load_file()?;
        file.secrets.insert(canonical_key, value.to_string());
        self.save_file(&file)
    }

    pub fn get(&self, scope: &SecretScope, name: &SecretName) -> Result<Option<String>> {
        let canonical_key = scope.canonical_key(name);
        let file = self.load_file_for_read()?;
        Ok(file.secrets.get(&canonical_key).cloned())
    }

    pub fn delete(&self, scope: &SecretScope, name: &SecretName) -> Result<bool> {
        if self.get(scope, name)?.is_none() {
            return Ok(false);
        }
        let _lock = self.acquire_lock(LockMode::Exclusive)?;
        #[cfg(windows)]
        self.recover_windows_atomic_write()?;
        let canonical_key = scope.canonical_key(name);
        let mut file = self.load_file()?;
        let removed = file.secrets.remove(&canonical_key).is_some();
        if removed {
            self.save_file(&file)?;
        }
        Ok(removed)
    }

    pub fn list(&self, scope_filter: Option<&SecretScope>) -> Result<Vec<SecretListEntry>> {
        let file = self.load_file_for_read()?;
        let mut entries = Vec::new();
        for canonical_key in file.secrets.keys() {
            let Some(entry) = parse_canonical_key(canonical_key) else {
                warn!("skipping invalid canonical secret key: {canonical_key}");
                continue;
            };
            if let Some(scope) = scope_filter
                && entry.scope != *scope
            {
                continue;
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    fn secrets_dir(&self) -> PathBuf {
        self.codex_home.join("secrets")
    }

    fn secrets_path(&self) -> PathBuf {
        self.secrets_dir().join(self.namespace.filename())
    }

    fn lock_path(&self) -> PathBuf {
        self.secrets_dir()
            .join(format!(".{}.lock", self.namespace.filename()))
    }

    fn acquire_lock(&self, mode: LockMode) -> Result<Option<fs::File>> {
        let path = self.lock_path();
        match mode {
            LockMode::Shared => {
                let file = match fs::OpenOptions::new().read(true).open(&path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let secrets_path = self.secrets_path();
                        if !secrets_path.try_exists().with_context(|| {
                            format!(
                                "failed to inspect secrets file at {}",
                                secrets_path.display()
                            )
                        })? {
                            return Ok(None);
                        }
                        let mut options = fs::OpenOptions::new();
                        options.create(true).read(true).write(true);
                        #[cfg(unix)]
                        options.mode(0o600);
                        match options.open(&path) {
                            Ok(file) => file,
                            Err(error) => {
                                debug!(
                                    "reading secrets without a shared lock because {} could not be created: {error}",
                                    path.display()
                                );
                                return Ok(None);
                            }
                        }
                    }
                    Err(error) => {
                        debug!(
                            "reading secrets without a shared lock because {} could not be opened: {error}",
                            path.display()
                        );
                        return Ok(None);
                    }
                };
                if let Err(error) = FileExt::lock_shared(&file) {
                    debug!(
                        "reading secrets without a shared lock because {} could not be locked: {error}",
                        path.display()
                    );
                    return Ok(None);
                }
                Ok(Some(file))
            }
            LockMode::Exclusive => {
                let dir = self.secrets_dir();
                fs::create_dir_all(&dir)
                    .with_context(|| format!("failed to create secrets dir {}", dir.display()))?;
                let mut options = fs::OpenOptions::new();
                options.create(true).read(true).write(true);
                #[cfg(unix)]
                options.mode(0o600);
                let file = options.open(&path).with_context(|| {
                    format!("failed to open secrets lock at {}", path.display())
                })?;
                FileExt::lock_exclusive(&file).with_context(|| {
                    format!("failed to lock secrets file at {}", path.display())
                })?;
                Ok(Some(file))
            }
        }
    }

    fn load_file_for_read(&self) -> Result<SecretsFile> {
        let read_lock = self.acquire_lock(LockMode::Shared)?;
        if read_lock.is_some() {
            return self.load_file();
        }
        let file = self.load_file();
        if read_lock.is_none()
            && self.lock_path().try_exists().unwrap_or(/*default*/ false)
            && let Some(_retry_lock) = self.acquire_lock(LockMode::Shared)?
        {
            return self.load_file();
        }
        file
    }

    fn load_file(&self) -> Result<SecretsFile> {
        let logical_path = self.secrets_path();
        #[cfg(windows)]
        let source_path = atomic_file::readable_path(&logical_path)?;
        #[cfg(not(windows))]
        let source_path = if logical_path.try_exists().with_context(|| {
            format!(
                "failed to inspect secrets file at {}",
                logical_path.display()
            )
        })? {
            Some(logical_path.clone())
        } else {
            None
        };
        let Some(source_path) = source_path else {
            return Ok(SecretsFile::new_empty());
        };

        let ciphertext = fs::read(&source_path)
            .with_context(|| format!("failed to read secrets file at {}", source_path.display()))?;
        let passphrase = self.load_passphrase()?.with_context(|| {
            format!(
                "secrets file exists at {} but its key is missing from the keyring",
                logical_path.display()
            )
        })?;
        let plaintext = decrypt_with_passphrase(&ciphertext, &passphrase)?;
        let mut parsed: SecretsFile = serde_json::from_slice(&plaintext).with_context(|| {
            format!(
                "failed to deserialize decrypted secrets file at {}",
                source_path.display()
            )
        })?;
        if parsed.version == 0 {
            parsed.version = SECRETS_VERSION;
        }
        anyhow::ensure!(
            parsed.version <= SECRETS_VERSION,
            "secrets file version {} is newer than supported version {}",
            parsed.version,
            SECRETS_VERSION
        );
        Ok(parsed)
    }

    fn save_file(&self, file: &SecretsFile) -> Result<()> {
        let dir = self.secrets_dir();
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create secrets dir {}", dir.display()))?;

        let passphrase = self.load_or_create_passphrase()?;
        let plaintext = serde_json::to_vec(file).context("failed to serialize secrets file")?;
        let ciphertext = encrypt_with_passphrase(&plaintext, &passphrase)?;
        let path = self.secrets_path();
        atomic_file::write_file_atomically(&path, &ciphertext)?;
        Ok(())
    }

    fn load_or_create_passphrase(&self) -> Result<SecretString> {
        if let Some(existing) = self.load_passphrase()? {
            return Ok(existing);
        }
        let account = compute_keyring_account_for_namespace(&self.codex_home, self.namespace);
        let generated = generate_passphrase()?;
        self.keyring_store
            .save(keyring_service(), &account, generated.expose_secret())
            .map_err(|err| anyhow::anyhow!(err.message()))
            .context("failed to persist secrets key in keyring")?;
        Ok(generated)
    }

    fn load_passphrase(&self) -> Result<Option<SecretString>> {
        let account = compute_keyring_account_for_namespace(&self.codex_home, self.namespace);
        let loaded = self
            .keyring_store
            .load(keyring_service(), &account)
            .map_err(|err| anyhow::anyhow!(err.message()))
            .with_context(|| format!("failed to load secrets key from keyring for {account}"))?;
        Ok(loaded.map(SecretString::from))
    }
}

impl SecretsBackend for LocalSecretsBackend {
    fn set(&self, scope: &SecretScope, name: &SecretName, value: &str) -> Result<()> {
        LocalSecretsBackend::set(self, scope, name, value)
    }

    fn get(&self, scope: &SecretScope, name: &SecretName) -> Result<Option<String>> {
        LocalSecretsBackend::get(self, scope, name)
    }

    fn delete(&self, scope: &SecretScope, name: &SecretName) -> Result<bool> {
        LocalSecretsBackend::delete(self, scope, name)
    }

    fn list(&self, scope_filter: Option<&SecretScope>) -> Result<Vec<SecretListEntry>> {
        LocalSecretsBackend::list(self, scope_filter)
    }
}

fn generate_passphrase() -> Result<SecretString> {
    let mut bytes = [0_u8; 32];
    let mut rng = OsRng;
    rng.try_fill_bytes(&mut bytes)
        .context("failed to generate random secrets key")?;
    // Base64 keeps the keyring payload ASCII-safe without reducing entropy.
    let encoded = BASE64_STANDARD.encode(bytes);
    wipe_bytes(&mut bytes);
    Ok(SecretString::from(encoded))
}

fn wipe_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        // Volatile writes make it much harder for the compiler to elide the wipe.
        // SAFETY: `byte` is a valid mutable reference into `bytes`.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

fn encrypt_with_passphrase(plaintext: &[u8], passphrase: &SecretString) -> Result<Vec<u8>> {
    let recipient = ScryptRecipient::new(passphrase.clone());
    encrypt(&recipient, plaintext).context("failed to encrypt secrets file")
}

fn decrypt_with_passphrase(ciphertext: &[u8], passphrase: &SecretString) -> Result<Vec<u8>> {
    let identity = ScryptIdentity::new(passphrase.clone());
    decrypt(&identity, ciphertext).context("failed to decrypt secrets file")
}

fn parse_canonical_key(canonical_key: &str) -> Option<SecretListEntry> {
    let mut parts = canonical_key.split('/');
    let scope_kind = parts.next()?;
    match scope_kind {
        "global" => {
            let name = parts.next()?;
            if parts.next().is_some() {
                return None;
            }
            let name = SecretName::new(name).ok()?;
            Some(SecretListEntry {
                scope: SecretScope::Global,
                name,
            })
        }
        "env" => {
            let environment_id = parts.next()?;
            let name = parts.next()?;
            if parts.next().is_some() {
                return None;
            }
            let name = SecretName::new(name).ok()?;
            let scope = SecretScope::environment(environment_id.to_string()).ok()?;
            Some(SecretListEntry { scope, name })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_keyring_store::CredentialStoreError;
    use codex_keyring_store::tests::MockKeyringStore;
    use keyring::Error as KeyringError;
    use pretty_assertions::assert_eq;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Barrier;
    use std::sync::Mutex;
    use std::sync::PoisonError;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[derive(Clone, Debug)]
    struct SlowMissingKeyringStore {
        inner: MockKeyringStore,
    }

    impl KeyringStore for SlowMissingKeyringStore {
        fn load(
            &self,
            service: &str,
            account: &str,
        ) -> std::result::Result<Option<String>, CredentialStoreError> {
            let loaded = self.inner.load(service, account)?;
            if loaded.is_none() {
                thread::sleep(Duration::from_millis(/*millis*/ 50));
            }
            Ok(loaded)
        }

        fn save(
            &self,
            service: &str,
            account: &str,
            value: &str,
        ) -> std::result::Result<(), CredentialStoreError> {
            self.inner.save(service, account, value)
        }

        fn delete(
            &self,
            service: &str,
            account: &str,
        ) -> std::result::Result<bool, CredentialStoreError> {
            self.inner.delete(service, account)
        }
    }

    #[derive(Clone, Debug)]
    struct BlockingLoadKeyringStore {
        inner: MockKeyringStore,
        block_next_load: Arc<AtomicBool>,
        load_started: mpsc::Sender<()>,
        load_release: Arc<Mutex<mpsc::Receiver<()>>>,
    }

    impl KeyringStore for BlockingLoadKeyringStore {
        fn load(
            &self,
            service: &str,
            account: &str,
        ) -> std::result::Result<Option<String>, CredentialStoreError> {
            let loaded = self.inner.load(service, account)?;
            if self.block_next_load.swap(/*val*/ false, Ordering::SeqCst) {
                self.load_started.send(()).map_err(|error| {
                    CredentialStoreError::new(KeyringError::Invalid(
                        "failed to signal blocked keyring load".into(),
                        error.to_string(),
                    ))
                })?;
                self.load_release
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv_timeout(Duration::from_secs(/*secs*/ 5))
                    .map_err(|error| {
                        CredentialStoreError::new(KeyringError::Invalid(
                            "blocked keyring load was not released".into(),
                            error.to_string(),
                        ))
                    })?;
            }
            Ok(loaded)
        }

        fn save(
            &self,
            service: &str,
            account: &str,
            value: &str,
        ) -> std::result::Result<(), CredentialStoreError> {
            self.inner.save(service, account, value)
        }

        fn delete(
            &self,
            service: &str,
            account: &str,
        ) -> std::result::Result<bool, CredentialStoreError> {
            self.inner.delete(service, account)
        }
    }

    #[test]
    fn load_file_rejects_newer_schema_versions() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let keyring = Arc::new(MockKeyringStore::default());
        let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring.clone());

        let file = SecretsFile {
            version: SECRETS_VERSION + 1,
            secrets: BTreeMap::new(),
        };
        backend.save_file(&file)?;
        let ciphertext = fs::read(backend.secrets_path())?;
        let account = compute_keyring_account_for_namespace(
            codex_home.path(),
            LocalSecretsNamespace::ManagedSecrets,
        );
        let passphrase = keyring.saved_value(&account);

        let error = backend
            .load_file()
            .expect_err("must reject newer schema version");
        assert!(
            error.to_string().contains("newer than supported version"),
            "unexpected error: {error:#}"
        );
        assert_eq!(fs::read(backend.secrets_path())?, ciphertext);
        assert_eq!(keyring.saved_value(&account), passphrase);
        Ok(())
    }

    #[test]
    fn set_fails_when_keyring_is_unavailable() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let keyring = Arc::new(MockKeyringStore::default());
        let account = compute_keyring_account_for_namespace(
            codex_home.path(),
            LocalSecretsNamespace::ManagedSecrets,
        );
        keyring.set_error(
            &account,
            KeyringError::Invalid("error".into(), "load".into()),
        );

        let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring);
        let scope = SecretScope::Global;
        let name = SecretName::new("TEST_SECRET")?;
        let error = backend
            .set(&scope, &name, "secret-value")
            .expect_err("must fail when keyring load fails");
        assert!(
            error
                .to_string()
                .contains("failed to load secrets key from keyring"),
            "unexpected error: {error:#}"
        );
        Ok(())
    }

    #[test]
    fn missing_file_operations_do_not_create_key_or_storage() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let keyring = Arc::new(MockKeyringStore::default());
        let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring.clone());
        let name = SecretName::new("TEST_SECRET")?;

        assert_eq!(backend.get(&SecretScope::Global, &name)?, None);
        assert_eq!(backend.list(/*scope_filter*/ None)?, Vec::new());
        assert!(!backend.delete(&SecretScope::Global, &name)?);
        let account = compute_keyring_account_for_namespace(
            codex_home.path(),
            LocalSecretsNamespace::ManagedSecrets,
        );
        assert!(!keyring.contains(&account));
        assert!(!backend.secrets_path().exists());
        assert!(!backend.lock_path().exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn existing_store_can_be_read_without_creating_a_lock_file() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let keyring = Arc::new(MockKeyringStore::default());
        let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring);
        let name = SecretName::new("TEST_SECRET")?;
        backend.set(&SecretScope::Global, &name, "secret")?;
        fs::remove_file(backend.lock_path())?;

        let secrets_dir = backend.secrets_dir();
        let original_permissions = fs::metadata(&secrets_dir)?.permissions();
        let mut read_only_permissions = original_permissions.clone();
        read_only_permissions.set_mode(/*mode*/ 0o500);
        fs::set_permissions(&secrets_dir, read_only_permissions)?;
        let read_result = (|| -> Result<_> {
            Ok((
                backend.get(&SecretScope::Global, &name)?,
                backend.list(/*scope_filter*/ None)?,
                backend.delete(&SecretScope::Global, &SecretName::new("MISSING")?)?,
            ))
        })();
        fs::set_permissions(&secrets_dir, original_permissions)?;

        let (value, entries, deleted) = read_result?;
        assert_eq!(value, Some("secret".to_string()));
        assert_eq!(
            entries,
            vec![SecretListEntry {
                scope: SecretScope::Global,
                name,
            }]
        );
        assert!(!deleted);
        assert!(!backend.lock_path().exists());
        Ok(())
    }

    #[test]
    fn missing_key_for_existing_file_preserves_ciphertext() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let keyring = Arc::new(MockKeyringStore::default());
        let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring.clone());
        let name = SecretName::new("TEST_SECRET")?;
        backend.set(&SecretScope::Global, &name, "secret")?;
        let path = backend.secrets_path();
        let ciphertext = fs::read(&path)?;
        let account = compute_keyring_account_for_namespace(
            codex_home.path(),
            LocalSecretsNamespace::ManagedSecrets,
        );
        assert!(keyring.delete(keyring_service(), &account)?);

        backend
            .get(&SecretScope::Global, &name)
            .expect_err("missing key must fail closed");
        assert!(!keyring.contains(&account));
        assert_eq!(fs::read(path)?, ciphertext);
        Ok(())
    }

    #[test]
    fn concurrent_first_writes_share_one_key_and_preserve_both_values() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let keyring = Arc::new(SlowMissingKeyringStore {
            inner: MockKeyringStore::default(),
        });
        let first_backend =
            LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring.clone());
        let second_backend = first_backend.clone();
        let barrier = Arc::new(Barrier::new(/*n*/ 3));

        let first_barrier = barrier.clone();
        let first = thread::spawn(move || {
            first_barrier.wait();
            first_backend.set(
                &SecretScope::Global,
                &SecretName::new("FIRST").expect("valid name"),
                "one",
            )
        });
        let second_barrier = barrier.clone();
        let second = thread::spawn(move || {
            second_barrier.wait();
            second_backend.set(
                &SecretScope::Global,
                &SecretName::new("SECOND").expect("valid name"),
                "two",
            )
        });
        barrier.wait();
        first.join().expect("first writer")?;
        second.join().expect("second writer")?;

        let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring);
        assert_eq!(
            backend.get(&SecretScope::Global, &SecretName::new("FIRST")?)?,
            Some("one".to_string())
        );
        assert_eq!(
            backend.get(&SecretScope::Global, &SecretName::new("SECOND")?)?,
            Some("two".to_string())
        );
        Ok(())
    }

    #[test]
    fn concurrent_set_and_delete_preserve_both_mutations() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let block_next_load = Arc::new(AtomicBool::new(/*v*/ false));
        let (load_started_sender, load_started_receiver) = mpsc::channel();
        let (load_release_sender, load_release_receiver) = mpsc::channel();
        let keyring = Arc::new(BlockingLoadKeyringStore {
            inner: MockKeyringStore::default(),
            block_next_load: block_next_load.clone(),
            load_started: load_started_sender,
            load_release: Arc::new(Mutex::new(load_release_receiver)),
        });
        let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring);
        let first_name = SecretName::new("FIRST")?;
        let second_name = SecretName::new("SECOND")?;
        backend.set(&SecretScope::Global, &first_name, "old")?;
        backend.set(&SecretScope::Global, &second_name, "two")?;

        block_next_load.store(/*val*/ true, Ordering::SeqCst);
        let set_backend = backend.clone();
        let set_name = first_name.clone();
        let set_thread =
            thread::spawn(move || set_backend.set(&SecretScope::Global, &set_name, "new"));
        load_started_receiver
            .recv_timeout(Duration::from_secs(/*secs*/ 5))
            .expect("set writer must reach keyring load");

        let delete_backend = backend.clone();
        let delete_name = second_name.clone();
        let (delete_sender, delete_receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = delete_sender.send(delete_backend.delete(&SecretScope::Global, &delete_name));
        });
        let delete_completed_early = delete_receiver
            .recv_timeout(Duration::from_millis(/*millis*/ 100))
            .ok()
            .transpose()?;
        load_release_sender.send(()).expect("release set writer");
        set_thread.join().expect("set writer")?;
        let deleted = match delete_completed_early {
            Some(deleted) => deleted,
            None => delete_receiver
                .recv_timeout(Duration::from_secs(/*secs*/ 30))
                .expect("delete writer")?,
        };

        assert!(deleted);
        assert_eq!(
            backend.get(&SecretScope::Global, &first_name)?,
            Some("new".to_string())
        );
        assert_eq!(backend.get(&SecretScope::Global, &second_name)?, None);
        Ok(())
    }

    #[test]
    fn local_namespaces_use_separate_files_and_keyring_accounts() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let keyring = Arc::new(MockKeyringStore::default());
        let managed_backend = LocalSecretsBackend::new_with_namespace(
            codex_home.path().to_path_buf(),
            keyring.clone(),
            LocalSecretsNamespace::ManagedSecrets,
        );
        let codex_auth_backend = LocalSecretsBackend::new_with_namespace(
            codex_home.path().to_path_buf(),
            keyring.clone(),
            LocalSecretsNamespace::CodexAuth,
        );
        let mcp_oauth_backend = LocalSecretsBackend::new_with_namespace(
            codex_home.path().to_path_buf(),
            keyring.clone(),
            LocalSecretsNamespace::McpOAuth,
        );
        let scope = SecretScope::Global;
        let name = SecretName::new("TEST_SECRET")?;

        managed_backend.set(&scope, &name, "managed")?;
        codex_auth_backend.set(&scope, &name, "codex-auth")?;
        mcp_oauth_backend.set(&scope, &name, "mcp-oauth")?;

        assert_eq!(
            managed_backend.get(&scope, &name)?,
            Some("managed".to_string())
        );
        assert_eq!(
            codex_auth_backend.get(&scope, &name)?,
            Some("codex-auth".to_string())
        );
        assert_eq!(
            mcp_oauth_backend.get(&scope, &name)?,
            Some("mcp-oauth".to_string())
        );
        let secrets_dir = codex_home.path().join("secrets");
        assert!(secrets_dir.join(LOCAL_SECRETS_FILENAME).exists());
        assert!(secrets_dir.join(CODEX_AUTH_SECRETS_FILENAME).exists());
        assert!(secrets_dir.join(MCP_OAUTH_SECRETS_FILENAME).exists());

        let managed_account = compute_keyring_account_for_namespace(
            codex_home.path(),
            LocalSecretsNamespace::ManagedSecrets,
        );
        let codex_auth_account = compute_keyring_account_for_namespace(
            codex_home.path(),
            LocalSecretsNamespace::CodexAuth,
        );
        let mcp_oauth_account = compute_keyring_account_for_namespace(
            codex_home.path(),
            LocalSecretsNamespace::McpOAuth,
        );
        assert_ne!(managed_account, codex_auth_account);
        assert_ne!(managed_account, mcp_oauth_account);
        assert_ne!(codex_auth_account, mcp_oauth_account);
        assert!(keyring.saved_value(&managed_account).is_some());
        assert!(keyring.saved_value(&codex_auth_account).is_some());
        assert!(keyring.saved_value(&mcp_oauth_account).is_some());
        Ok(())
    }
}

#[cfg(all(test, windows))]
#[path = "local_windows_tests.rs"]
mod windows_tests;
