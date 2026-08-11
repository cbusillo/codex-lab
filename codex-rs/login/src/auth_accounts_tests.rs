use super::*;
use crate::auth_profiles::record_auth_profile_login;
use crate::token_data::IdTokenInfo;
use base64::Engine;
use codex_keyring_store::tests::MockKeyringStore;
use keyring::Error as KeyringError;
use pretty_assertions::assert_eq;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use tempfile::TempDir;

const TEST_AUTH_CREDENTIALS_STORE_MODE: AuthCredentialsStoreMode = AuthCredentialsStoreMode::File;
const TEST_AUTH_KEYRING_BACKEND_KIND: AuthKeyringBackendKind = AuthKeyringBackendKind::Direct;

fn compute_login_aggregate_keyring_account(codex_home: &Path) -> String {
    let canonical = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let short = hex.get(..16).unwrap_or(hex.as_str());
    format!("secrets|login-aggregate|{short}")
}

#[test]
fn file_mode_account_catalog_write_does_not_access_keyring() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    let keyring = Arc::new(MockKeyringStore::default());
    keyring.set_error(
        &compute_login_aggregate_keyring_account(codex_home.path()),
        KeyringError::Invalid("file mode".into(), "keyring access".into()),
    );
    let expected = AccountsFile::default();

    super::write_accounts_file_with_keyring_store(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        &expected,
        keyring,
    )?;

    assert_eq!(
        super::read_accounts_file(codex_home.path(), AuthCredentialsStoreMode::File)?,
        expected
    );
    assert!(
        !codex_home
            .path()
            .join("secrets/login_aggregate.age")
            .exists()
    );
    Ok(())
}

fn activate_account(
    codex_home: &Path,
    account_id: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<StoredAccount> {
    super::activate_account(
        codex_home,
        account_id,
        auth_credentials_store_mode,
        TEST_AUTH_KEYRING_BACKEND_KIND,
    )
}

#[allow(deprecated)]
fn commit_active_account(
    codex_home: &Path,
    account_id: &str,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<StoredAccount> {
    super::commit_active_account(
        codex_home,
        account_id,
        auth,
        auth_credentials_store_mode,
        TEST_AUTH_KEYRING_BACKEND_KIND,
    )
}

fn clear_active_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<()> {
    super::clear_active_account(
        codex_home,
        auth_credentials_store_mode,
        TEST_AUTH_KEYRING_BACKEND_KIND,
    )
}

fn save_auth(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<()> {
    crate::save_auth(
        codex_home,
        auth,
        auth_credentials_store_mode,
        TEST_AUTH_KEYRING_BACKEND_KIND,
    )
}

fn load_auth_dot_json(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<Option<AuthDotJson>> {
    crate::load_auth_dot_json(
        codex_home,
        auth_credentials_store_mode,
        TEST_AUTH_KEYRING_BACKEND_KIND,
    )
}

fn compare_and_swap_catalog_account_auth(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    catalog_id: &str,
    expected: &AuthDotJson,
    replacement: &AuthDotJson,
) -> io::Result<bool> {
    super::compare_and_swap_catalog_account_auth(
        codex_home,
        auth_credentials_store_mode,
        TEST_AUTH_KEYRING_BACKEND_KIND,
        catalog_id,
        expected,
        replacement,
    )
}

fn list_accounts(codex_home: &Path) -> io::Result<Vec<StoredAccount>> {
    super::list_accounts(codex_home, TEST_AUTH_CREDENTIALS_STORE_MODE)
}

fn get_active_account_id(codex_home: &Path) -> io::Result<Option<String>> {
    super::get_active_account_id(codex_home, TEST_AUTH_CREDENTIALS_STORE_MODE)
}

fn find_account(codex_home: &Path, account_id: &str) -> io::Result<Option<StoredAccount>> {
    super::find_account(codex_home, TEST_AUTH_CREDENTIALS_STORE_MODE, account_id)
}

fn update_account_last_refresh(
    codex_home: &Path,
    account_id: &str,
    last_refresh: DateTime<Utc>,
) -> io::Result<Option<StoredAccount>> {
    super::update_account_last_refresh(
        codex_home,
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        account_id,
        last_refresh,
    )
}

fn set_active_account_id(
    codex_home: &Path,
    account_id: Option<String>,
) -> io::Result<Option<StoredAccount>> {
    super::set_active_account_id(codex_home, TEST_AUTH_CREDENTIALS_STORE_MODE, account_id)
}

fn remove_account(codex_home: &Path, account_id: &str) -> io::Result<Option<StoredAccount>> {
    super::remove_account(codex_home, TEST_AUTH_CREDENTIALS_STORE_MODE, account_id)
}

fn remove_account_matching_credentials(
    codex_home: &Path,
    mode: AuthMode,
    openai_api_key: Option<&str>,
    tokens: Option<&TokenData>,
) -> io::Result<Option<StoredAccount>> {
    super::remove_account_matching_credentials(
        codex_home,
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        mode,
        openai_api_key,
        tokens,
    )
}

fn upsert_api_key_account(
    codex_home: &Path,
    api_key: String,
    label: Option<String>,
    make_active: bool,
) -> io::Result<StoredAccount> {
    super::upsert_api_key_account(
        codex_home,
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        api_key,
        label,
        make_active,
    )
}

fn upsert_chatgpt_account(
    codex_home: &Path,
    tokens: TokenData,
    last_refresh: DateTime<Utc>,
    label: Option<String>,
    make_active: bool,
) -> io::Result<StoredAccount> {
    super::upsert_chatgpt_account(
        codex_home,
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        tokens,
        last_refresh,
        label,
        make_active,
    )
}

fn make_chatgpt_tokens(account_id: Option<&str>, email: Option<&str>) -> TokenData {
    fn fake_jwt(account_id: Option<&str>, email: Option<&str>) -> String {
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }

        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let payload = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id.unwrap_or("acct"),
                "chatgpt_user_id": "user-12345",
                "user_id": "user-12345"
            }
        });
        let b64 = |value: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(value).expect("json to vec"))
        };
        let header_b64 = b64(&serde_json::to_value(header).expect("header value"));
        let payload_b64 = b64(&payload);
        let signature_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"sig");
        format!("{header_b64}.{payload_b64}.{signature_b64}")
    }

    TokenData {
        id_token: IdTokenInfo {
            email: email.map(str::to_string),
            chatgpt_plan_type: None,
            chatgpt_user_id: Some("user-12345".to_string()),
            chatgpt_account_id: account_id.map(str::to_string),
            chatgpt_account_is_fedramp: false,
            raw_jwt: fake_jwt(account_id, email),
        },
        access_token: "access".to_string(),
        refresh_token: "refresh".to_string(),
        account_id: account_id.map(str::to_string),
    }
}

fn make_chatgpt_tokens_with_claim_only_account_id(
    account_id: Option<&str>,
    email: Option<&str>,
) -> TokenData {
    let mut tokens = make_chatgpt_tokens(account_id, email);
    tokens.account_id = None;
    tokens
}

#[test]
fn store_mode_plumbing_preserves_shared_file_catalog() {
    let temp = TempDir::new().expect("tempdir");
    let stored = super::upsert_api_key_account(
        temp.path(),
        AuthCredentialsStoreMode::Ephemeral,
        "sk-mode-plumbing".to_string(),
        Some("Mode plumbing".to_string()),
        /*make_active*/ true,
    )
    .expect("upsert account");

    for mode in [
        AuthCredentialsStoreMode::File,
        AuthCredentialsStoreMode::Keyring,
        AuthCredentialsStoreMode::Auto,
        AuthCredentialsStoreMode::Ephemeral,
    ] {
        assert_eq!(
            vec![stored.clone()],
            super::list_accounts(temp.path(), mode).expect("list accounts")
        );
        assert_eq!(
            Some(stored.id.clone()),
            super::get_active_account_id(temp.path(), mode).expect("active account id")
        );
    }

    assert!(accounts_file_path(temp.path()).is_file());
}

#[test]
fn missing_accounts_file_defaults_to_empty_state() {
    let temp = TempDir::new().expect("tempdir");

    assert_eq!(
        Vec::<StoredAccount>::new(),
        list_accounts(temp.path()).expect("list accounts")
    );
    assert_eq!(
        None,
        get_active_account_id(temp.path()).expect("active account id")
    );
}

#[test]
fn empty_accounts_file_defaults_to_empty_state() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(accounts_file_path(temp.path()), "\n \t").expect("write empty accounts file");

    assert_eq!(
        Vec::<StoredAccount>::new(),
        list_accounts(temp.path()).expect("list accounts")
    );
    assert_eq!(
        None,
        get_active_account_id(temp.path()).expect("active account id")
    );
}

#[test]
fn upsert_api_key_creates_dedupes_and_sets_active() {
    let temp = TempDir::new().expect("tempdir");
    let first = upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        Some("Work".to_string()),
        /*make_active*/ true,
    )
    .expect("upsert api key");

    let second = upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        Some("Updated".to_string()),
        /*make_active*/ false,
    )
    .expect("upsert same key");

    assert_eq!(first.id, second.id);
    assert_eq!(Some("Updated"), second.label.as_deref());
    assert_eq!(
        Some(first.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );

    let mut expected = second;
    expected.last_used_at = first.last_used_at;
    assert_eq!(
        vec![expected],
        list_accounts(temp.path()).expect("list accounts")
    );
}

#[test]
fn upsert_chatgpt_dedupes_by_account_id_and_email() {
    let temp = TempDir::new().expect("tempdir");
    let first = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens(Some("acct-1"), Some("USER@example.com")),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert chatgpt");

    let second = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens(Some("acct-1"), Some("user@example.com")),
        Utc::now(),
        Some("Personal".to_string()),
        /*make_active*/ false,
    )
    .expect("update chatgpt");

    assert_eq!(first.id, second.id);
    assert_eq!(Some("Personal"), second.label.as_deref());
    assert_eq!(1, list_accounts(temp.path()).expect("list accounts").len());
}

#[test]
fn only_successful_login_upsert_clears_reauth_required() {
    let temp = TempDir::new().expect("tempdir");
    let tokens = make_chatgpt_tokens(Some("acct-reauth"), Some("user@example.com"));
    let stored = upsert_chatgpt_account(
        temp.path(),
        tokens.clone(),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("store account");
    let (_, expected_auth) =
        auth_for_account(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE, &stored.id)
            .expect("load account auth");
    assert!(
        super::mark_account_reauth_required_if_auth_matches(
            temp.path(),
            TEST_AUTH_CREDENTIALS_STORE_MODE,
            Some(&stored.id),
            &expected_auth,
        )
        .expect("mark account")
    );

    let synced = upsert_chatgpt_account(
        temp.path(),
        tokens.clone(),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("sync account");
    assert_eq!(synced.health, AccountHealth::ReauthRequired);
    let repaired = super::upsert_chatgpt_account_after_login(
        temp.path(),
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        tokens,
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("record successful login");
    assert_eq!(repaired.id, stored.id);
    assert_eq!(repaired.health, AccountHealth::Ok);
}

#[test]
fn terminal_mark_ignores_chatgpt_api_key_metadata_but_rejects_stale_tokens() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens(Some("acct-reauth"), Some("user@example.com")),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("store account");
    let (_, mut expected_auth) =
        auth_for_account(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE, &stored.id)
            .expect("load account auth");
    expected_auth.openai_api_key = Some("ephemeral-login-key".to_string());
    assert!(
        super::mark_account_reauth_required_if_auth_matches(
            temp.path(),
            TEST_AUTH_CREDENTIALS_STORE_MODE,
            Some(&stored.id),
            &expected_auth,
        )
        .expect("mark account")
    );

    let repaired = super::upsert_chatgpt_account_after_login(
        temp.path(),
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        stored.tokens.expect("stored tokens"),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("repair account");
    let (_, mut stale_auth) =
        auth_for_account(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE, &repaired.id)
            .expect("load repaired auth");
    stale_auth.tokens.as_mut().expect("tokens").refresh_token = "stale-refresh-token".to_string();
    assert!(
        !super::mark_account_reauth_required_if_auth_matches(
            temp.path(),
            TEST_AUTH_CREDENTIALS_STORE_MODE,
            Some(&repaired.id),
            &stale_auth,
        )
        .expect("reject stale mark")
    );
    assert_eq!(
        find_account(temp.path(), &repaired.id)
            .expect("read account")
            .expect("account")
            .health,
        AccountHealth::Ok
    );
}

#[test]
fn account_health_defaults_to_ok_for_legacy_catalog_entries() {
    let temp = TempDir::new().expect("tempdir");
    std::fs::write(
        temp.path().join(ACCOUNTS_FILE_NAME),
        r#"{"version":1,"accounts":[{"id":"api-key:legacy","mode":"apikey","openai_api_key":"sk-legacy"}]}"#,
    )
    .expect("write legacy account catalog");

    let accounts = list_accounts(temp.path()).expect("read legacy account catalog");
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].health, AccountHealth::Ok);
}

#[test]
fn find_chatgpt_account_by_tokens_finds_matching_non_active_account() {
    let temp = TempDir::new().expect("tempdir");
    upsert_api_key_account(
        temp.path(),
        "sk-active".to_string(),
        Some("Active".to_string()),
        /*make_active*/ true,
    )
    .expect("insert active api key");
    let tokens = make_chatgpt_tokens(Some("acct-1"), Some("user@example.com"));
    let chatgpt = upsert_chatgpt_account(
        temp.path(),
        tokens.clone(),
        Utc::now(),
        Some("ChatGPT".to_string()),
        /*make_active*/ false,
    )
    .expect("insert chatgpt");

    assert_eq!(
        Some(chatgpt),
        super::find_chatgpt_account_by_tokens(
            temp.path(),
            TEST_AUTH_CREDENTIALS_STORE_MODE,
            &tokens,
        )
        .expect("find chatgpt")
    );
}

#[test]
fn find_api_key_account_by_key_finds_matching_non_active_account() {
    let temp = TempDir::new().expect("tempdir");
    upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens(Some("acct-active"), Some("user@example.com")),
        Utc::now(),
        Some("Active".to_string()),
        /*make_active*/ true,
    )
    .expect("insert active chatgpt");
    let api_key = upsert_api_key_account(
        temp.path(),
        "sk-saved".to_string(),
        Some("API".to_string()),
        /*make_active*/ false,
    )
    .expect("insert api key");

    assert_eq!(
        Some(api_key),
        super::find_api_key_account_by_key(
            temp.path(),
            TEST_AUTH_CREDENTIALS_STORE_MODE,
            "sk-saved",
        )
        .expect("find api key")
    );
}

#[test]
fn upsert_chatgpt_dedupes_by_id_token_account_id_without_email() {
    let temp = TempDir::new().expect("tempdir");
    let first = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens_with_claim_only_account_id(Some("acct-1"), /*email*/ None),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert chatgpt");

    let second_tokens =
        make_chatgpt_tokens_with_claim_only_account_id(Some("acct-1"), /*email*/ None);
    let second = upsert_chatgpt_account(
        temp.path(),
        second_tokens.clone(),
        Utc::now(),
        Some("Workspace".to_string()),
        /*make_active*/ false,
    )
    .expect("update chatgpt");

    assert_eq!(first.id, second.id);
    assert_eq!(1, list_accounts(temp.path()).expect("list accounts").len());
    assert_eq!(
        Some(second),
        remove_account_matching_credentials(
            temp.path(),
            AuthMode::Chatgpt,
            /*openai_api_key*/ None,
            Some(&second_tokens),
        )
        .expect("remove by claim-only account id")
    );
}

#[test]
fn chatgpt_accounts_with_same_email_but_different_ids_are_distinct() {
    let temp = TempDir::new().expect("tempdir");
    let personal = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens(Some("acct-personal"), Some("user@example.com")),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert personal account");
    let team = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens(Some("acct-team"), Some("user@example.com")),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("insert team account");

    assert_ne!(personal.id, team.id);
    assert_eq!(2, list_accounts(temp.path()).expect("list accounts").len());
}

#[test]
fn set_active_account_id_records_and_touches_account() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("upsert api key");
    assert_eq!(None, stored.last_used_at);

    let activated = set_active_account_id(temp.path(), Some(stored.id.clone()))
        .expect("set active")
        .expect("activated account");

    assert_eq!(stored.id, activated.id);
    assert!(activated.last_used_at.is_some());
    assert_eq!(
        Some(stored.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
}

#[test]
fn set_active_account_id_does_not_persist_missing_id() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("upsert api key");

    assert_eq!(
        None,
        set_active_account_id(temp.path(), Some("missing".to_string()))
            .expect("set missing active")
    );

    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
    assert_eq!(
        vec![stored],
        list_accounts(temp.path()).expect("list accounts")
    );
}

#[test]
fn activate_api_key_account_writes_auth_and_marks_active() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        Some("Work".to_string()),
        /*make_active*/ false,
    )
    .expect("upsert api key");

    let activated = activate_account(temp.path(), &stored.id, AuthCredentialsStoreMode::File)
        .expect("activate account");

    assert_eq!(stored.id, activated.id);
    assert!(activated.last_used_at.is_some());
    assert_eq!(
        Some(stored.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
    assert_eq!(
        crate::AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("sk-test".to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        },
        load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File)
            .expect("read auth json")
            .expect("auth json should exist")
    );
}

#[test]
#[allow(deprecated)]
fn commit_active_account_writes_stored_auth_and_marks_active() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        Some("Work".to_string()),
        /*make_active*/ false,
    )
    .expect("upsert api key");

    let stale_caller_auth = crate::AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-stale-caller".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
    };
    let activated = commit_active_account(
        temp.path(),
        &stored.id,
        &stale_caller_auth,
        TEST_AUTH_CREDENTIALS_STORE_MODE,
    )
    .expect("commit active account");

    assert_eq!(stored.id, activated.id);
    assert!(activated.last_used_at.is_some());
    assert_eq!(
        Some(stored.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
    assert_eq!(
        crate::AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("sk-test".to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        },
        load_auth_dot_json(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE)
            .expect("read auth json")
            .expect("auth json should exist")
    );
}

#[test]
fn auth_for_account_returns_auth_without_persisting_activation() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        Some("Work".to_string()),
        /*make_active*/ false,
    )
    .expect("upsert api key");

    let (account, auth) =
        super::auth_for_account(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE, &stored.id)
            .expect("account auth");

    assert_eq!(stored, account);
    assert_eq!(
        crate::AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("sk-test".to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        },
        auth
    );
    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
    assert_eq!(
        None,
        load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File).expect("read auth json")
    );
}

#[test]
fn commit_active_account_leaves_existing_state_unchanged_when_account_is_missing() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-previous".to_string(),
        Some("Previous".to_string()),
        /*make_active*/ true,
    )
    .expect("upsert previous api key");
    let previous_auth = crate::AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-previous".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
    };
    save_auth(temp.path(), &previous_auth, AuthCredentialsStoreMode::File)
        .expect("save previous auth");
    let err = activate_account(temp.path(), "missing", AuthCredentialsStoreMode::File)
        .expect_err("missing account should fail");

    assert_eq!(io::ErrorKind::Other, err.kind());
    assert_eq!(
        Some(stored.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
    assert_eq!(
        previous_auth,
        load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File)
            .expect("read auth json")
            .expect("auth json should exist")
    );
}

#[test]
fn commit_active_account_preserves_stored_accounts_without_existing_auth() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-new".to_string(),
        Some("New".to_string()),
        /*make_active*/ false,
    )
    .expect("upsert api key");
    let err = activate_account(temp.path(), "missing", AuthCredentialsStoreMode::File)
        .expect_err("missing account should fail");

    assert_eq!(io::ErrorKind::Other, err.kind());
    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
    assert_eq!(
        None,
        load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File).expect("read auth json")
    );
    assert_eq!(
        vec![stored],
        list_accounts(temp.path()).expect("list accounts")
    );
}

#[test]
fn commit_active_account_restores_auth_when_accounts_write_fails() {
    let temp = TempDir::new().expect("tempdir");
    let previous = upsert_api_key_account(
        temp.path(),
        "sk-previous".to_string(),
        Some("Previous".to_string()),
        /*make_active*/ true,
    )
    .expect("upsert previous account");
    let target = upsert_api_key_account(
        temp.path(),
        "sk-target".to_string(),
        Some("Target".to_string()),
        /*make_active*/ false,
    )
    .expect("upsert target account");
    let previous_auth = crate::AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-previous".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
    };
    save_auth(
        temp.path(),
        &previous_auth,
        TEST_AUTH_CREDENTIALS_STORE_MODE,
    )
    .expect("save previous auth");
    let accounts_before = list_accounts(temp.path()).expect("list accounts before failure");

    for attempt in 0..100 {
        fs::write(
            temp.path().join(format!(
                ".{ACCOUNTS_FILE_NAME}.{}.{}.tmp",
                std::process::id(),
                attempt
            )),
            "occupied",
        )
        .expect("occupy accounts temp path");
    }

    let err = activate_account(temp.path(), &target.id, TEST_AUTH_CREDENTIALS_STORE_MODE)
        .expect_err("accounts write should fail");

    assert_eq!(io::ErrorKind::Other, err.kind());
    assert_eq!(
        accounts_before,
        list_accounts(temp.path()).expect("list accounts")
    );
    assert_eq!(
        Some(previous.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
    assert_eq!(
        previous_auth,
        load_auth_dot_json(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE)
            .expect("read auth json")
            .expect("auth json should exist")
    );
}

#[test]
fn activate_chatgpt_account_writes_auth_and_marks_active() {
    let temp = TempDir::new().expect("tempdir");
    let last_refresh = Utc::now();
    let tokens = make_chatgpt_tokens(Some("acct-activate"), Some("user@example.com"));
    let stored = upsert_chatgpt_account(
        temp.path(),
        tokens.clone(),
        last_refresh,
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("upsert chatgpt");

    let activated = activate_account(temp.path(), &stored.id, AuthCredentialsStoreMode::File)
        .expect("activate account");

    assert_eq!(stored.id, activated.id);
    assert_eq!(
        Some(stored.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
    assert_eq!(
        crate::AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(tokens),
            last_refresh: Some(last_refresh),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        },
        load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File)
            .expect("read auth json")
            .expect("auth json should exist")
    );
}

#[test]
fn activate_missing_account_leaves_active_account_unchanged() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("upsert api key");

    let err = activate_account(temp.path(), "missing", AuthCredentialsStoreMode::File)
        .expect_err("missing account should fail");

    assert_eq!(io::ErrorKind::Other, err.kind());
    assert_eq!(
        Some(stored.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
}

#[test]
fn remove_account_clears_active() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens(Some("acct-remove"), Some("user@example.com")),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert chatgpt");

    assert_eq!(
        Some(stored.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );

    assert_eq!(
        Some(stored.clone()),
        remove_account(temp.path(), &stored.id).expect("remove")
    );
    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
    assert_eq!(
        Vec::<StoredAccount>::new(),
        list_accounts(temp.path()).expect("list accounts")
    );
}

#[test]
fn remove_account_promotes_remaining_account_when_active_is_removed() {
    let temp = TempDir::new().expect("tempdir");
    let active = upsert_api_key_account(
        temp.path(),
        "sk-active".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert active account");
    let fallback = upsert_api_key_account(
        temp.path(),
        "sk-fallback".to_string(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("insert fallback account");

    let active_id = active.id.clone();
    assert_eq!(
        Some(active),
        remove_account(temp.path(), &active_id).expect("remove active account")
    );
    assert_eq!(
        Some(fallback.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
    let promoted = find_account(temp.path(), &fallback.id)
        .expect("find promoted account")
        .expect("promoted account");
    assert!(promoted.last_used_at.is_some());
}

#[test]
fn clear_active_account_removes_active_marker_and_auth_file() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-active".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert active account");
    save_auth(
        temp.path(),
        &crate::AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("sk-active".to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("write auth file");

    assert_eq!(
        Some(stored.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );

    clear_active_account(temp.path(), AuthCredentialsStoreMode::File).expect("clear active");

    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
    assert_eq!(
        None,
        load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File)
            .expect("auth should be readable")
    );
}

#[test]
fn remove_account_matching_credentials_removes_api_key_or_chatgpt_account() {
    let temp = TempDir::new().expect("tempdir");
    let api = upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert api account");
    let tokens = make_chatgpt_tokens(Some("acct-chatgpt"), Some("user@example.com"));
    let chatgpt = upsert_chatgpt_account(
        temp.path(),
        tokens.clone(),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("insert chatgpt account");

    assert_eq!(
        Some(api),
        remove_account_matching_credentials(
            temp.path(),
            AuthMode::ApiKey,
            Some("sk-test"),
            /*tokens*/ None,
        )
        .expect("remove api key")
    );
    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
    let removed_chatgpt = remove_account_matching_credentials(
        temp.path(),
        AuthMode::Chatgpt,
        /*openai_api_key*/ None,
        Some(&tokens),
    )
    .expect("remove chatgpt")
    .expect("removed chatgpt");
    assert_eq!(chatgpt.id, removed_chatgpt.id);
    assert_eq!(
        Vec::<StoredAccount>::new(),
        list_accounts(temp.path()).expect("list accounts")
    );
}

#[test]
fn remove_account_matching_credentials_does_not_create_an_empty_catalog() {
    let temp = TempDir::new().expect("tempdir");

    assert_eq!(
        None,
        remove_account_matching_credentials(
            temp.path(),
            AuthMode::ApiKey,
            Some("sk-missing"),
            /*tokens*/ None,
        )
        .expect("remove missing account")
    );
    assert!(!temp.path().join("auth_accounts.json").exists());
}

#[test]
fn remove_account_matching_credentials_clears_active_account_and_preserves_remaining_account() {
    let temp = TempDir::new().expect("tempdir");
    let active_tokens = make_chatgpt_tokens(Some("acct-active"), Some("active@example.com"));
    let active = upsert_chatgpt_account(
        temp.path(),
        active_tokens.clone(),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert active chatgpt account");
    let fallback = upsert_api_key_account(
        temp.path(),
        "sk-fallback".to_string(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("insert fallback account");

    assert_eq!(
        Some(active),
        remove_account_matching_credentials(
            temp.path(),
            AuthMode::Chatgpt,
            /*openai_api_key*/ None,
            Some(&active_tokens),
        )
        .expect("remove active chatgpt")
    );
    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
    assert_eq!(
        Some(fallback.clone()),
        find_account(temp.path(), &fallback.id).expect("find remaining account")
    );
}

#[test]
fn update_account_last_refresh_updates_only_target_account() {
    let temp = TempDir::new().expect("tempdir");
    let first = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens(Some("acct-first"), Some("first@example.com")),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("insert first");
    let second = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens(Some("acct-second"), Some("second@example.com")),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("insert second");
    let refresh = Utc::now();

    let updated = update_account_last_refresh(temp.path(), &second.id, refresh)
        .expect("update refresh")
        .expect("updated account");

    assert_eq!(Some(refresh), updated.last_refresh);
    assert_eq!(
        None,
        update_account_last_refresh(temp.path(), "missing", refresh).expect("missing update")
    );
    let first_after = find_account(temp.path(), &first.id)
        .expect("find first")
        .expect("first account");
    assert_eq!(first.last_refresh, first_after.last_refresh);
}

#[test]
fn compare_and_swap_catalog_account_auth_syncs_active_auth() {
    let temp = TempDir::new().expect("tempdir");
    let initial_tokens = make_chatgpt_tokens(Some("acct-active"), Some("user@example.com"));
    let account = upsert_chatgpt_account(
        temp.path(),
        initial_tokens,
        Utc::now() - chrono::Duration::days(1),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("store active account");
    let (_stored, expected) =
        auth_for_account(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE, &account.id)
            .expect("load stored auth");
    save_auth(temp.path(), &expected, TEST_AUTH_CREDENTIALS_STORE_MODE).expect("save active auth");
    let mut replacement = expected.clone();
    let replacement_tokens = replacement.tokens.as_mut().expect("replacement tokens");
    replacement_tokens.access_token = "updated-access".to_string();
    replacement_tokens.refresh_token = "updated-refresh".to_string();
    replacement.last_refresh = Some(Utc::now());

    assert!(
        compare_and_swap_catalog_account_auth(
            temp.path(),
            TEST_AUTH_CREDENTIALS_STORE_MODE,
            &account.id,
            &expected,
            &replacement,
        )
        .expect("compare and swap auth")
    );
    assert_eq!(
        replacement,
        load_auth_dot_json(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE)
            .expect("load active auth")
            .expect("active auth should exist")
    );
    assert_eq!(
        replacement,
        auth_for_account(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE, &account.id,)
            .expect("load catalog auth")
            .1
    );
}

#[test]
fn compare_and_swap_catalog_account_auth_preserves_concurrent_login() {
    let temp = TempDir::new().expect("tempdir");
    let initial_tokens = make_chatgpt_tokens(Some("acct-active"), Some("user@example.com"));
    let account = upsert_chatgpt_account(
        temp.path(),
        initial_tokens,
        Utc::now() - chrono::Duration::days(1),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("store active account");
    let (_stored, expected) =
        auth_for_account(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE, &account.id)
            .expect("load stored auth");
    save_auth(temp.path(), &expected, TEST_AUTH_CREDENTIALS_STORE_MODE).expect("save active auth");

    let concurrent_login = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-concurrent-login".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
    };
    save_auth(
        temp.path(),
        &concurrent_login,
        TEST_AUTH_CREDENTIALS_STORE_MODE,
    )
    .expect("save concurrent login before catalog mirror");

    let mut replacement = expected.clone();
    replacement
        .tokens
        .as_mut()
        .expect("replacement tokens")
        .access_token = "stale-refresh".to_string();
    assert!(
        !compare_and_swap_catalog_account_auth(
            temp.path(),
            TEST_AUTH_CREDENTIALS_STORE_MODE,
            &account.id,
            &expected,
            &replacement,
        )
        .expect("compare and swap auth")
    );
    assert_eq!(
        concurrent_login,
        load_auth_dot_json(temp.path(), TEST_AUTH_CREDENTIALS_STORE_MODE)
            .expect("load active auth")
            .expect("active auth should exist")
    );
}

#[test]
fn account_store_ignores_existing_auth_profiles_until_import_exists() {
    let temp = TempDir::new().expect("tempdir");
    record_auth_profile_login(
        temp.path(),
        "work",
        Some("acct-profile".to_string()),
        Some("profile@example.com".to_string()),
    )
    .expect("record auth profile login");

    assert_eq!(
        Vec::<StoredAccount>::new(),
        list_accounts(temp.path()).expect("list accounts")
    );
    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
}

#[test]
fn recovers_from_trailing_json_documents_by_keeping_latest_accounts_file() {
    let temp = TempDir::new().expect("tempdir");
    let path = accounts_file_path(temp.path());

    let first = AccountsFile {
        version: default_version(),
        active_account_id: Some("first-active".to_string()),
        accounts: vec![StoredAccount {
            id: "first-active".to_string(),
            mode: AuthMode::ApiKey,
            label: Some("first".to_string()),
            openai_api_key: Some("sk-first".to_string()),
            tokens: None,
            last_refresh: None,
            created_at: None,
            last_used_at: None,
            health: AccountHealth::Ok,
        }],
    };
    let second = AccountsFile {
        version: default_version(),
        active_account_id: Some("second-active".to_string()),
        accounts: vec![StoredAccount {
            id: "second-active".to_string(),
            mode: AuthMode::ApiKey,
            label: Some("second".to_string()),
            openai_api_key: Some("sk-second".to_string()),
            tokens: None,
            last_refresh: None,
            created_at: None,
            last_used_at: None,
            health: AccountHealth::Ok,
        }],
    };

    let first_json = serde_json::to_string_pretty(&first).expect("serialize first");
    let second_json = serde_json::to_string_pretty(&second).expect("serialize second");
    fs::write(&path, format!("{first_json}\n{second_json}\n"))
        .expect("write corrupt accounts file");

    assert_eq!(
        second.accounts,
        list_accounts(temp.path()).expect("recover accounts")
    );
    assert_eq!(
        Some("second-active"),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
    assert_eq!(
        second_json,
        fs::read_to_string(&path).expect("read repaired accounts file")
    );
}

#[cfg(unix)]
#[test]
fn saved_accounts_file_is_private_after_rewrite() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");

    upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("upsert api account");
    let path = accounts_file_path(temp.path());
    let mut permissions = fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o666);
    fs::set_permissions(&path, permissions).expect("make accounts file permissive");

    upsert_api_key_account(
        temp.path(),
        "sk-other".to_string(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("rewrite accounts file");

    let mode = fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(0o600, mode);
}

#[test]
fn concurrent_account_upserts_preserve_all_updates() {
    let temp = TempDir::new().expect("tempdir");
    let codex_home = temp.path().to_path_buf();
    let worker_count = 16;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(worker_count));
    let workers = (0..worker_count)
        .map(|index| {
            let codex_home = codex_home.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                super::upsert_api_key_account(
                    &codex_home,
                    TEST_AUTH_CREDENTIALS_STORE_MODE,
                    format!("sk-concurrent-{index}"),
                    /*label*/ None,
                    /*make_active*/ false,
                )
                .expect("upsert account");
            })
        })
        .collect::<Vec<_>>();

    for worker in workers {
        worker.join().expect("worker should finish");
    }

    assert_eq!(
        super::list_accounts(&codex_home, TEST_AUTH_CREDENTIALS_STORE_MODE)
            .expect("list accounts")
            .len(),
        worker_count
    );
}

#[test]
fn inactive_profile_sync_does_not_replace_active_account_credentials() {
    let temp = TempDir::new().expect("tempdir");
    let initial_tokens = make_chatgpt_tokens(Some("acct-active"), Some("user@example.com"));
    let active = upsert_chatgpt_account(
        temp.path(),
        initial_tokens.clone(),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("store active account");
    let mut profile_tokens = initial_tokens.clone();
    profile_tokens.access_token = "profile-access".to_string();
    profile_tokens.refresh_token = "profile-refresh".to_string();

    assert_eq!(
        super::upsert_inactive_chatgpt_account(
            temp.path(),
            TEST_AUTH_CREDENTIALS_STORE_MODE,
            profile_tokens,
            Utc::now(),
            /*label*/ None,
        )
        .expect("sync inactive account"),
        None
    );
    assert_eq!(
        find_account(temp.path(), &active.id)
            .expect("find active account")
            .and_then(|account| account.tokens),
        Some(initial_tokens)
    );
}

#[test]
fn inactive_chatgpt_sync_inserts_a_nonactive_account() {
    let temp = TempDir::new().expect("tempdir");
    let active = upsert_api_key_account(
        temp.path(),
        "sk-active".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("store active account");
    let tokens = make_chatgpt_tokens(Some("acct-inactive"), Some("user@example.com"));
    let last_refresh = Utc::now();

    let inserted = super::upsert_inactive_chatgpt_account(
        temp.path(),
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        tokens.clone(),
        last_refresh,
        Some("Profile".to_string()),
    )
    .expect("sync inactive account")
    .expect("insert inactive account");
    let inserted_id = inserted.id.clone();

    assert_eq!(AuthMode::Chatgpt, inserted.mode);
    assert_eq!(Some(tokens), inserted.tokens);
    assert_eq!(Some(last_refresh), inserted.last_refresh);
    assert_eq!(Some("Profile".to_string()), inserted.label);
    assert_eq!(None, inserted.last_used_at);
    assert_eq!(
        Some(active.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
    assert_eq!(
        Some(inserted),
        find_account(temp.path(), &inserted_id).expect("find inactive account")
    );
}

#[test]
fn inactive_chatgpt_sync_updates_a_matching_inactive_account() {
    let temp = TempDir::new().expect("tempdir");
    let active = upsert_api_key_account(
        temp.path(),
        "sk-active".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("store active account");
    let initial_tokens = make_chatgpt_tokens(Some("acct-inactive"), Some("user@example.com"));
    let initial = upsert_chatgpt_account(
        temp.path(),
        initial_tokens,
        Utc::now() - chrono::Duration::days(1),
        Some("Original".to_string()),
        /*make_active*/ false,
    )
    .expect("store inactive account");
    let mut replacement_tokens =
        make_chatgpt_tokens(Some("acct-inactive"), Some("user@example.com"));
    replacement_tokens.access_token = "updated-access".to_string();
    replacement_tokens.refresh_token = "updated-refresh".to_string();
    let last_refresh = Utc::now();

    let updated = super::upsert_inactive_chatgpt_account(
        temp.path(),
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        replacement_tokens.clone(),
        last_refresh,
        Some("Updated".to_string()),
    )
    .expect("sync inactive account")
    .expect("update inactive account");

    assert_eq!(initial.id, updated.id);
    assert_eq!(Some(replacement_tokens), updated.tokens);
    assert_eq!(Some(last_refresh), updated.last_refresh);
    assert_eq!(Some("Updated".to_string()), updated.label);
    assert_eq!(initial.created_at, updated.created_at);
    assert_eq!(initial.last_used_at, updated.last_used_at);
    assert_eq!(
        Some(active.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
    assert_eq!(
        Some(updated),
        find_account(temp.path(), &initial.id).expect("find inactive account")
    );
}

#[test]
fn same_workspace_different_users_remain_separate_accounts() {
    let temp = TempDir::new().expect("tempdir");
    let first_tokens = make_chatgpt_tokens(Some("shared-workspace"), /*email*/ None);
    upsert_chatgpt_account(
        temp.path(),
        first_tokens,
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("store first user");
    let mut second_tokens = make_chatgpt_tokens(Some("shared-workspace"), /*email*/ None);
    second_tokens.id_token.chatgpt_user_id = Some("user-67890".to_string());

    upsert_chatgpt_account(
        temp.path(),
        second_tokens,
        Utc::now(),
        /*label*/ None,
        /*make_active*/ false,
    )
    .expect("store second user");

    assert_eq!(list_accounts(temp.path()).expect("list accounts").len(), 2);
}
