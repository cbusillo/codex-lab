use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::atomic::compiler_fence;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

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
use tracing::warn;

use super::SecretListEntry;
use super::SecretName;
use super::SecretScope;
use super::SecretsBackend;
use super::compute_keyring_account_for_namespace;
use super::keyring_service;

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
        let canonical_key = scope.canonical_key(name);
        let mut file = self.load_file()?;
        file.secrets.insert(canonical_key, value.to_string());
        self.save_file(&file)
    }

    pub fn get(&self, scope: &SecretScope, name: &SecretName) -> Result<Option<String>> {
        if !self.secrets_path().exists() {
            return Ok(None);
        }
        let _lock = self.acquire_lock(LockMode::Shared)?;
        let canonical_key = scope.canonical_key(name);
        let file = self.load_file()?;
        Ok(file.secrets.get(&canonical_key).cloned())
    }

    pub fn delete(&self, scope: &SecretScope, name: &SecretName) -> Result<bool> {
        let _lock = self.acquire_lock(LockMode::Exclusive)?;
        let canonical_key = scope.canonical_key(name);
        let mut file = self.load_file()?;
        let removed = file.secrets.remove(&canonical_key).is_some();
        if removed {
            self.save_file(&file)?;
        }
        Ok(removed)
    }

    pub fn list(&self, scope_filter: Option<&SecretScope>) -> Result<Vec<SecretListEntry>> {
        if !self.secrets_path().exists() {
            return Ok(Vec::new());
        }
        let _lock = self.acquire_lock(LockMode::Shared)?;
        let file = self.load_file()?;
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

    fn acquire_lock(&self, mode: LockMode) -> Result<fs::File> {
        let dir = self.secrets_dir();
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create secrets dir {}", dir.display()))?;
        let path = self.lock_path();
        let mut options = fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(&path)
            .with_context(|| format!("failed to open secrets lock at {}", path.display()))?;
        match mode {
            LockMode::Shared => FileExt::lock_shared(&file),
            LockMode::Exclusive => FileExt::lock_exclusive(&file),
        }
        .with_context(|| format!("failed to lock secrets file at {}", path.display()))?;
        Ok(file)
    }

    fn load_file(&self) -> Result<SecretsFile> {
        let path = self.secrets_path();
        if !path.exists() {
            return Ok(SecretsFile::new_empty());
        }

        let ciphertext = fs::read(&path)
            .with_context(|| format!("failed to read secrets file at {}", path.display()))?;
        let passphrase = self.load_passphrase()?.with_context(|| {
            format!(
                "secrets file exists at {} but its key is missing from the keyring",
                path.display()
            )
        })?;
        let plaintext = decrypt_with_passphrase(&ciphertext, &passphrase)?;
        let mut parsed: SecretsFile = serde_json::from_slice(&plaintext).with_context(|| {
            format!(
                "failed to deserialize decrypted secrets file at {}",
                path.display()
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
        write_file_atomically(&path, &ciphertext)?;
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

fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().with_context(|| {
        format!(
            "failed to compute parent directory for secrets file at {}",
            path.display()
        )
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let filename = path.file_name().with_context(|| {
        format!(
            "failed to compute filename for secrets file at {}",
            path.display()
        )
    })?;
    let tmp_path = dir.join(format!(
        ".{}.tmp-{}-{nonce}",
        filename.to_string_lossy(),
        std::process::id()
    ));

    {
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut tmp_file = options.open(&tmp_path).with_context(|| {
            format!(
                "failed to create temp secrets file at {}",
                tmp_path.display()
            )
        })?;
        tmp_file.write_all(contents).with_context(|| {
            format!(
                "failed to write temp secrets file at {}",
                tmp_path.display()
            )
        })?;
        tmp_file.sync_all().with_context(|| {
            format!("failed to sync temp secrets file at {}", tmp_path.display())
        })?;
    }

    if let Err(error) = replace_file(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(error).with_context(|| {
            format!(
                "failed to atomically replace secrets file at {} with {}",
                path.display(),
                tmp_path.display()
            )
        });
    }
    if let Err(err) = sync_parent_directory(dir) {
        warn!("failed to sync secrets directory after atomic replace: {err:#}");
    }
    Ok(())
}

fn replace_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(not(windows))]
    {
        fs::rename(src, dst)
    }
    #[cfg(windows)]
    {
        match replace_file_windows(src, dst) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => fs::rename(src, dst),
            Err(err) => Err(err),
        }
    }
}

#[cfg(windows)]
fn replace_file_windows(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    const REPLACEFILE_IGNORE_MERGE_ERRORS: u32 = 0x0000_0002;

    unsafe extern "system" {
        fn ReplaceFileW(
            lp_replaced_file_name: *const u16,
            lp_replacement_file_name: *const u16,
            lp_backup_file_name: *const u16,
            dw_replace_flags: u32,
            lp_exclude: *mut c_void,
            lp_reserved: *mut c_void,
        ) -> i32;
    }

    let dst_wide: Vec<u16> = dst.as_os_str().encode_wide().chain(Some(0)).collect();
    let src_wide: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        ReplaceFileW(
            dst_wide.as_ptr(),
            src_wide.as_ptr(),
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

fn sync_parent_directory(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(dir)
            .with_context(|| format!("failed to open secrets dir {} for sync", dir.display()))?
            .sync_all()
            .with_context(|| format!("failed to sync secrets dir {}", dir.display()))?;
    }
    Ok(())
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
    use std::sync::Barrier;
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
                thread::sleep(Duration::from_millis(50));
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
        let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring);

        let file = SecretsFile {
            version: SECRETS_VERSION + 1,
            secrets: BTreeMap::new(),
        };
        backend.save_file(&file)?;

        let error = backend
            .load_file()
            .expect_err("must reject newer schema version");
        assert!(
            error.to_string().contains("newer than supported version"),
            "unexpected error: {error:#}"
        );
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
    fn save_file_does_not_leave_temp_files() -> Result<()> {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let keyring = Arc::new(MockKeyringStore::default());
        let backend = LocalSecretsBackend::new(codex_home.path().to_path_buf(), keyring);

        let scope = SecretScope::Global;
        let name = SecretName::new("TEST_SECRET")?;
        backend.set(&scope, &name, "one")?;
        backend.set(&scope, &name, "two")?;

        let secrets_dir = backend.secrets_dir();
        let entries = fs::read_dir(&secrets_dir)
            .with_context(|| format!("failed to read {}", secrets_dir.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("failed to enumerate {}", secrets_dir.display()))?;

        let filenames: Vec<String> = entries
            .into_iter()
            .filter_map(|entry| entry.file_name().to_str().map(ToString::to_string))
            .collect();
        assert!(filenames.contains(&LOCAL_SECRETS_FILENAME.to_string()));
        assert!(
            filenames.contains(&format!(".{LOCAL_SECRETS_FILENAME}.lock")),
            "missing persistent lock file: {filenames:?}"
        );
        assert!(
            filenames.iter().all(|filename| !filename.contains(".tmp-")),
            "unexpected temp file: {filenames:?}"
        );
        assert_eq!(backend.get(&scope, &name)?, Some("two".to_string()));
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
        let barrier = Arc::new(Barrier::new(3));

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
    fn failed_atomic_replace_preserves_destination_and_cleans_temp_file() -> Result<()> {
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("destination");
        fs::create_dir(&destination)?;

        write_file_atomically(&destination, b"secret")
            .expect_err("replacing a directory should fail");

        assert!(destination.is_dir());
        let entries = fs::read_dir(dir.path())?.collect::<std::io::Result<Vec<_>>>()?;
        assert_eq!(
            entries
                .into_iter()
                .map(|entry| entry.file_name())
                .collect::<Vec<_>>(),
            vec![
                destination
                    .file_name()
                    .expect("destination filename")
                    .to_owned()
            ]
        );
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
