use crate::token_data::TokenData;
use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_keyring_store::DefaultKeyringStore;
use codex_keyring_store::KeyringStore;
use rand::RngCore;
use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;

use crate::auth::AuthDotJson;
use crate::auth::encrypted_aggregate::with_invalidated_encrypted_aggregate;
use crate::auth::save_auth;

const ACCOUNTS_FILE_NAME: &str = "auth_accounts.json";
pub(crate) const ACCOUNTS_FILE_VERSION: u32 = 1;
static CATALOG_REFRESH_WRITE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredAccount {
    pub id: String,
    pub mode: AuthMode,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_api_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenData>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
pub(crate) struct AccountsFile {
    #[serde(default = "default_version")]
    pub(crate) version: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) active_account_id: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) accounts: Vec<StoredAccount>,
}

impl Default for AccountsFile {
    fn default() -> Self {
        Self {
            version: default_version(),
            active_account_id: None,
            accounts: Vec::new(),
        }
    }
}

fn default_version() -> u32 {
    ACCOUNTS_FILE_VERSION
}

fn accounts_file_path(codex_home: &Path) -> PathBuf {
    codex_home.join(ACCOUNTS_FILE_NAME)
}

fn accounts_file_path_for_mode(
    codex_home: &Path,
    _auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> PathBuf {
    accounts_file_path(codex_home)
}

fn read_accounts_file(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<AccountsFile> {
    let path = accounts_file_path_for_mode(codex_home, auth_credentials_store_mode);
    match File::open(path) {
        Ok(mut file) => {
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            let (parsed, repaired) = parse_accounts_file(&contents)?;
            if repaired {
                write_accounts_file(codex_home, auth_credentials_store_mode, &parsed)?;
            }
            Ok(parsed)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(AccountsFile::default()),
        Err(err) => Err(err),
    }
}

pub(crate) fn read_accounts_file_for_migration(
    codex_home: &Path,
) -> io::Result<(AccountsFile, bool)> {
    let path = accounts_file_path(codex_home);
    match File::open(path) {
        Ok(mut file) => {
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            let (parsed, _repaired) = parse_accounts_file(&contents)?;
            if parsed.version != ACCOUNTS_FILE_VERSION {
                return Err(io::Error::other(format!(
                    "unsupported auth accounts version {}",
                    parsed.version
                )));
            }
            let catalog_present = parsed != AccountsFile::default();
            Ok((parsed, catalog_present))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok((AccountsFile::default(), false)),
        Err(err) => Err(err),
    }
}

fn parse_accounts_file(contents: &str) -> io::Result<(AccountsFile, bool)> {
    if contents.trim().is_empty() {
        return Ok((AccountsFile::default(), false));
    }

    match serde_json::from_str(contents) {
        Ok(parsed) => Ok((parsed, false)),
        Err(original_err) => {
            let mut latest: Option<AccountsFile> = None;
            let mut recovered_count = 0;
            let stream = serde_json::Deserializer::from_str(contents).into_iter::<AccountsFile>();
            for value in stream {
                match value {
                    Ok(parsed) => {
                        recovered_count += 1;
                        latest = Some(parsed);
                    }
                    Err(_) => return Err(original_err.into()),
                }
            }

            match (recovered_count, latest) {
                (count, Some(parsed)) if count > 1 => Ok((parsed, true)),
                _ => Err(original_err.into()),
            }
        }
    }
}

fn write_accounts_file(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    data: &AccountsFile,
) -> io::Result<()> {
    let keyring_store: Arc<dyn KeyringStore> = Arc::new(DefaultKeyringStore);
    write_accounts_file_with_keyring_store(
        codex_home,
        auth_credentials_store_mode,
        data,
        keyring_store,
    )
}

fn write_accounts_file_with_keyring_store(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    data: &AccountsFile,
    keyring_store: Arc<dyn KeyringStore>,
) -> io::Result<()> {
    let path = accounts_file_path_for_mode(codex_home, auth_credentials_store_mode);
    with_invalidated_encrypted_aggregate(
        codex_home,
        auth_credentials_store_mode,
        keyring_store,
        || write_accounts_file_legacy(&path, data),
    )
}

#[cfg(test)]
pub(crate) fn write_accounts_file_with_keyring_store_for_tests(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    data: &AccountsFile,
    keyring_store: Arc<dyn KeyringStore>,
) -> io::Result<()> {
    write_accounts_file_with_keyring_store(
        codex_home,
        auth_credentials_store_mode,
        data,
        keyring_store,
    )
}

fn write_accounts_file_legacy(path: &Path, data: &AccountsFile) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("accounts path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;

    let raw = serde_json::to_string_pretty(data).map_err(io::Error::other)?;
    let (tmp_path, mut file) = create_accounts_tmp_file(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(raw.as_bytes())?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    if let Err(err) = replace_accounts_file(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

fn create_accounts_tmp_file(path: &Path) -> io::Result<(PathBuf, fs::File)> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(ACCOUNTS_FILE_NAME);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for attempt in 0..100 {
        let pid = std::process::id();
        let tmp_path = parent.join(format!(".{file_name}.{pid}.{attempt}.tmp"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&tmp_path) {
            Ok(file) => return Ok((tmp_path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "failed to allocate temporary accounts path for {}",
            path.display()
        ),
    ))
}

fn replace_accounts_file(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        fs::rename(src, dst)
    }
    #[cfg(windows)]
    {
        match replace_file_windows(src, dst) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => fs::rename(src, dst),
            Err(err) => Err(err),
        }
    }
}

#[cfg(windows)]
fn replace_file_windows(src: &Path, dst: &Path) -> io::Result<()> {
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
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn next_id() -> String {
    let mut bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_supported_chatgpt_mode(mode: AuthMode) -> bool {
    matches!(mode, AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens)
}

fn chatgpt_account_id(tokens: &TokenData) -> Option<&str> {
    tokens
        .account_id
        .as_deref()
        .or(tokens.id_token.chatgpt_account_id.as_deref())
}

fn match_chatgpt_account(existing: &StoredAccount, tokens: &TokenData) -> bool {
    if !is_supported_chatgpt_mode(existing.mode) {
        return false;
    }

    let existing_tokens = match &existing.tokens {
        Some(tokens) => tokens,
        None => return false,
    };

    match (
        chatgpt_account_id(existing_tokens),
        chatgpt_account_id(tokens),
    ) {
        (Some(a), Some(b)) if a == b => {
            match (
                existing_tokens.id_token.email.as_ref(),
                tokens.id_token.email.as_ref(),
            ) {
                (Some(a), Some(b)) => normalize_email(a) == normalize_email(b),
                _ => true,
            }
        }
        (Some(_), Some(_)) => false,
        _ => false,
    }
}

fn match_api_key_account(existing: &StoredAccount, api_key: &str) -> bool {
    existing.mode == AuthMode::ApiKey
        && existing
            .openai_api_key
            .as_ref()
            .is_some_and(|stored| stored == api_key)
}

fn touch_account(account: &mut StoredAccount, used: bool) {
    if account.created_at.is_none() {
        account.created_at = Some(now());
    }
    if used {
        account.last_used_at = Some(now());
    }
}

fn upsert_account(
    mut data: AccountsFile,
    mut new_account: StoredAccount,
) -> (AccountsFile, StoredAccount) {
    let existing_idx = match new_account.mode {
        AuthMode::ApiKey => new_account.openai_api_key.as_ref().and_then(|api_key| {
            data.accounts
                .iter()
                .position(|acc| match_api_key_account(acc, api_key))
        }),
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => {
            new_account.tokens.as_ref().and_then(|tokens| {
                data.accounts
                    .iter()
                    .position(|acc| match_chatgpt_account(acc, tokens))
            })
        }
        AuthMode::AgentIdentity | AuthMode::PersonalAccessToken => None,
    };

    if let Some(idx) = existing_idx {
        let mut account = data.accounts[idx].clone();
        if new_account.label.is_some() {
            account.label = new_account.label;
        }
        if new_account.last_refresh.is_some() {
            account.last_refresh = new_account.last_refresh;
        }
        if let Some(tokens) = new_account.tokens {
            account.tokens = Some(tokens);
        }
        if let Some(api_key) = new_account.openai_api_key {
            account.openai_api_key = Some(api_key);
        }
        if let Some(last_used) = new_account.last_used_at {
            account.last_used_at = Some(last_used);
        }
        data.accounts[idx] = account.clone();
        return (data, account);
    }

    if new_account.created_at.is_none() {
        new_account.created_at = Some(now());
    }

    data.accounts.push(new_account.clone());
    (data, new_account)
}

fn select_fallback_active_account(data: &mut AccountsFile) {
    if let Some(account) = data.accounts.first_mut() {
        data.active_account_id = Some(account.id.clone());
        touch_account(account, true);
    } else {
        data.active_account_id = None;
    }
}

pub fn list_accounts(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<Vec<StoredAccount>> {
    let data = read_accounts_file(codex_home, auth_credentials_store_mode)?;
    Ok(data.accounts)
}

pub fn get_active_account_id(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<Option<String>> {
    let data = read_accounts_file(codex_home, auth_credentials_store_mode)?;
    Ok(data.active_account_id)
}

pub fn find_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    account_id: &str,
) -> io::Result<Option<StoredAccount>> {
    let data = read_accounts_file(codex_home, auth_credentials_store_mode)?;
    Ok(data.accounts.into_iter().find(|acc| acc.id == account_id))
}

fn find_preferred_matching_account(
    data: &AccountsFile,
    matches: impl Fn(&StoredAccount) -> bool,
) -> Option<StoredAccount> {
    if let Some(active_account_id) = data.active_account_id.as_deref()
        && let Some(account) = data
            .accounts
            .iter()
            .find(|account| account.id == active_account_id && matches(account))
    {
        return Some(account.clone());
    }

    data.accounts
        .iter()
        .find(|account| matches(account))
        .cloned()
}

pub fn find_chatgpt_account_by_tokens(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    tokens: &TokenData,
) -> io::Result<Option<StoredAccount>> {
    let data = read_accounts_file(codex_home, auth_credentials_store_mode)?;
    Ok(find_preferred_matching_account(&data, |account| {
        match_chatgpt_account(account, tokens)
    }))
}

pub fn find_api_key_account_by_key(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    api_key: &str,
) -> io::Result<Option<StoredAccount>> {
    let data = read_accounts_file(codex_home, auth_credentials_store_mode)?;
    Ok(find_preferred_matching_account(&data, |account| {
        match_api_key_account(account, api_key)
    }))
}

pub fn auth_for_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    account_id: &str,
) -> io::Result<(StoredAccount, AuthDotJson)> {
    let account = find_account(codex_home, auth_credentials_store_mode, account_id)?
        .ok_or_else(|| io::Error::other(format!("account with id {account_id} was not found")))?;

    let auth =
        match account.mode {
            AuthMode::ApiKey => AuthDotJson {
                auth_mode: Some(AuthMode::ApiKey),
                openai_api_key: Some(account.openai_api_key.clone().ok_or_else(|| {
                    io::Error::other("stored API key account is missing the key value")
                })?),
                tokens: None,
                last_refresh: None,
                agent_identity: None,
                personal_access_token: None,
            },
            AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => AuthDotJson {
                auth_mode: Some(account.mode),
                openai_api_key: None,
                tokens: Some(account.tokens.clone().ok_or_else(|| {
                    io::Error::other("stored ChatGPT account is missing token data")
                })?),
                last_refresh: account.last_refresh,
                agent_identity: None,
                personal_access_token: None,
            },
            AuthMode::AgentIdentity => {
                return Err(io::Error::other(
                    "stored agent identity account activation is not supported",
                ));
            }
            AuthMode::PersonalAccessToken => {
                return Err(io::Error::other(
                    "stored personal access token account activation is not supported",
                ));
            }
        };

    Ok((account, auth))
}

pub(crate) fn update_catalog_account_from_auth(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    catalog_id: &str,
    auth: &AuthDotJson,
) -> io::Result<()> {
    let _guard = CATALOG_REFRESH_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut data = read_accounts_file(codex_home, auth_credentials_store_mode)?;
    let account = data
        .accounts
        .iter_mut()
        .find(|account| account.id == catalog_id)
        .ok_or_else(|| io::Error::other(format!("catalog account {catalog_id} was not found")))?;

    match auth.resolved_mode() {
        AuthMode::ApiKey => {
            account.mode = AuthMode::ApiKey;
            account.openai_api_key = auth.openai_api_key.clone();
            account.tokens = None;
            account.last_refresh = None;
        }
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => {
            account.mode = auth.resolved_mode();
            account.openai_api_key = None;
            account.tokens = auth.tokens.clone();
            account.last_refresh = auth.last_refresh;
        }
        AuthMode::AgentIdentity | AuthMode::PersonalAccessToken => {
            return Err(io::Error::other(
                "catalog account storage does not support this authentication mode",
            ));
        }
    }
    write_accounts_file(codex_home, auth_credentials_store_mode, &data)
}

pub fn update_account_last_refresh(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    account_id: &str,
    last_refresh: DateTime<Utc>,
) -> io::Result<Option<StoredAccount>> {
    let mut data = read_accounts_file(codex_home, auth_credentials_store_mode)?;

    let Some(account) = data.accounts.iter_mut().find(|acc| acc.id == account_id) else {
        return Ok(None);
    };
    account.last_refresh = Some(last_refresh);
    let updated = account.clone();
    write_accounts_file(codex_home, auth_credentials_store_mode, &data)?;
    Ok(Some(updated))
}

pub fn set_active_account_id(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    account_id: Option<String>,
) -> io::Result<Option<StoredAccount>> {
    let mut data = read_accounts_file(codex_home, auth_credentials_store_mode)?;

    let mut updated = None;
    if let Some(id) = account_id
        && let Some(account) = data.accounts.iter_mut().find(|acc| acc.id == id)
    {
        data.active_account_id = Some(id);
        touch_account(account, true);
        updated = Some(account.clone());
    } else {
        data.active_account_id = None;
    }
    write_accounts_file(codex_home, auth_credentials_store_mode, &data)?;
    Ok(updated)
}

/// Activate a stored account by writing its credentials to `auth.json` and
/// marking it active in the account store.
pub fn activate_account(
    codex_home: &Path,
    account_id: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<StoredAccount> {
    let (_account, auth) = auth_for_account(codex_home, auth_credentials_store_mode, account_id)?;
    commit_active_account(codex_home, account_id, &auth, auth_credentials_store_mode)
}

fn restore_previous_auth(
    codex_home: &Path,
    previous_auth: Option<AuthDotJson>,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<()> {
    if let Some(previous_auth) = previous_auth {
        save_auth(codex_home, &previous_auth, auth_credentials_store_mode)
    } else {
        crate::delete_auth(codex_home, auth_credentials_store_mode).map(|_| ())
    }
}

fn restore_previous_activation(
    codex_home: &Path,
    previous_auth: Option<AuthDotJson>,
    previous_active_account_id: Option<String>,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<()> {
    let auth_result = restore_previous_auth(codex_home, previous_auth, auth_credentials_store_mode);
    let active_account_result = set_active_account_id(
        codex_home,
        auth_credentials_store_mode,
        previous_active_account_id,
    );

    auth_result?;
    active_account_result?;
    Ok(())
}

pub fn commit_active_account(
    codex_home: &Path,
    account_id: &str,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<StoredAccount> {
    let previous_auth = crate::load_auth_dot_json(codex_home, auth_credentials_store_mode)?;
    let previous_active_account_id =
        get_active_account_id(codex_home, auth_credentials_store_mode)?;

    save_auth(codex_home, auth, auth_credentials_store_mode)?;
    match set_active_account_id(
        codex_home,
        auth_credentials_store_mode,
        Some(account_id.to_string()),
    ) {
        Ok(Some(activated)) => Ok(activated),
        Ok(None) => {
            let rollback_result = restore_previous_activation(
                codex_home,
                previous_auth,
                previous_active_account_id,
                auth_credentials_store_mode,
            );
            if let Err(rollback_err) = rollback_result {
                tracing::warn!(
                    "failed to roll back missing stored account activation: {rollback_err}"
                );
            }
            Err(io::Error::other(format!(
                "account with id {account_id} disappeared before activation"
            )))
        }
        Err(err) => {
            if let Err(rollback_err) = restore_previous_activation(
                codex_home,
                previous_auth,
                previous_active_account_id,
                auth_credentials_store_mode,
            ) {
                tracing::warn!(
                    "failed to roll back stored account activation error: {rollback_err}"
                );
            }
            Err(err)
        }
    }
}

pub fn remove_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    account_id: &str,
) -> io::Result<Option<StoredAccount>> {
    let mut data = read_accounts_file(codex_home, auth_credentials_store_mode)?;

    let removed = if let Some(pos) = data.accounts.iter().position(|acc| acc.id == account_id) {
        Some(data.accounts.remove(pos))
    } else {
        None
    };

    if data
        .active_account_id
        .as_ref()
        .is_some_and(|active| active == account_id)
    {
        select_fallback_active_account(&mut data);
    }

    write_accounts_file(codex_home, auth_credentials_store_mode, &data)?;
    Ok(removed)
}

pub fn clear_active_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<()> {
    set_active_account_id(codex_home, auth_credentials_store_mode, None)?;
    crate::delete_auth(codex_home, auth_credentials_store_mode).map(|_| ())
}

pub fn remove_account_matching_credentials(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    mode: AuthMode,
    openai_api_key: Option<&str>,
    tokens: Option<&TokenData>,
) -> io::Result<Option<StoredAccount>> {
    let mut data = read_accounts_file(codex_home, auth_credentials_store_mode)?;

    let removed = match mode {
        AuthMode::ApiKey => openai_api_key.and_then(|api_key| {
            data.accounts
                .iter()
                .position(|account| match_api_key_account(account, api_key))
        }),
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => tokens.and_then(|tokens| {
            data.accounts
                .iter()
                .position(|account| match_chatgpt_account(account, tokens))
        }),
        AuthMode::AgentIdentity | AuthMode::PersonalAccessToken => None,
    }
    .map(|pos| data.accounts.remove(pos));

    if let Some(removed) = &removed
        && data
            .active_account_id
            .as_ref()
            .is_some_and(|active| active == &removed.id)
    {
        data.active_account_id = None;
    }

    write_accounts_file(codex_home, auth_credentials_store_mode, &data)?;
    Ok(removed)
}

pub fn upsert_api_key_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    api_key: String,
    label: Option<String>,
    make_active: bool,
) -> io::Result<StoredAccount> {
    let data = read_accounts_file(codex_home, auth_credentials_store_mode)?;

    let new_account = StoredAccount {
        id: next_id(),
        mode: AuthMode::ApiKey,
        label,
        openai_api_key: Some(api_key),
        tokens: None,
        last_refresh: None,
        created_at: None,
        last_used_at: None,
    };

    let (mut data, mut stored) = upsert_account(data, new_account);

    if make_active {
        data.active_account_id = Some(stored.id.clone());
        if let Some(account) = data.accounts.iter_mut().find(|acc| acc.id == stored.id) {
            touch_account(account, true);
            stored = account.clone();
        }
    }

    write_accounts_file(codex_home, auth_credentials_store_mode, &data)?;
    Ok(stored)
}

pub(crate) fn insert_api_key_account_if_missing(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    api_key: String,
    label: Option<String>,
) -> io::Result<Option<StoredAccount>> {
    let mut data = read_accounts_file(codex_home, auth_credentials_store_mode)?;

    if data
        .accounts
        .iter()
        .any(|account| match_api_key_account(account, &api_key))
    {
        return Ok(None);
    }

    let mut account = StoredAccount {
        id: next_id(),
        mode: AuthMode::ApiKey,
        label,
        openai_api_key: Some(api_key),
        tokens: None,
        last_refresh: None,
        created_at: None,
        last_used_at: None,
    };
    touch_account(&mut account, false);
    data.accounts.push(account.clone());
    write_accounts_file(codex_home, auth_credentials_store_mode, &data)?;
    Ok(Some(account))
}

pub fn upsert_chatgpt_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    tokens: TokenData,
    last_refresh: DateTime<Utc>,
    label: Option<String>,
    make_active: bool,
) -> io::Result<StoredAccount> {
    let data = read_accounts_file(codex_home, auth_credentials_store_mode)?;

    let new_account = StoredAccount {
        id: next_id(),
        mode: AuthMode::Chatgpt,
        label,
        openai_api_key: None,
        tokens: Some(tokens),
        last_refresh: Some(last_refresh),
        created_at: None,
        last_used_at: None,
    };

    let (mut data, mut stored) = upsert_account(data, new_account);

    if make_active {
        data.active_account_id = Some(stored.id.clone());
        if let Some(account) = data.accounts.iter_mut().find(|acc| acc.id == stored.id) {
            touch_account(account, true);
            stored = account.clone();
        }
    }

    write_accounts_file(codex_home, auth_credentials_store_mode, &data)?;
    Ok(stored)
}

pub(crate) fn insert_chatgpt_account_if_missing(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    tokens: TokenData,
    last_refresh: DateTime<Utc>,
    label: Option<String>,
) -> io::Result<Option<StoredAccount>> {
    let mut data = read_accounts_file(codex_home, auth_credentials_store_mode)?;

    if data
        .accounts
        .iter()
        .any(|account| match_chatgpt_account(account, &tokens))
    {
        return Ok(None);
    }

    let mut account = StoredAccount {
        id: next_id(),
        mode: AuthMode::Chatgpt,
        label,
        openai_api_key: None,
        tokens: Some(tokens),
        last_refresh: Some(last_refresh),
        created_at: None,
        last_used_at: None,
    };
    touch_account(&mut account, false);
    data.accounts.push(account.clone());
    write_accounts_file(codex_home, auth_credentials_store_mode, &data)?;
    Ok(Some(account))
}

#[cfg(test)]
#[path = "auth_accounts_tests.rs"]
mod tests;
