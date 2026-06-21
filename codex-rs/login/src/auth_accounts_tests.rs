use super::*;
use crate::auth_profiles::record_auth_profile_login;
use crate::token_data::IdTokenInfo;
use base64::Engine;
use pretty_assertions::assert_eq;
use serde::Serialize;
use tempfile::TempDir;

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
fn upsert_chatgpt_dedupes_by_id_token_account_id_without_email() {
    let temp = TempDir::new().expect("tempdir");
    let first = upsert_chatgpt_account(
        temp.path(),
        make_chatgpt_tokens_with_claim_only_account_id(Some("acct-1"), None),
        Utc::now(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert chatgpt");

    let second_tokens = make_chatgpt_tokens_with_claim_only_account_id(Some("acct-1"), None);
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
        },
        crate::load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File)
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

    let (account, auth) = auth_for_account(temp.path(), &stored.id).expect("account auth");

    assert_eq!(stored, account);
    assert_eq!(
        crate::AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some("sk-test".to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
        },
        auth
    );
    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
    assert_eq!(
        None,
        crate::load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File)
            .expect("read auth json")
    );
}

#[test]
fn commit_active_account_rolls_back_auth_and_active_id_when_account_is_missing() {
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
    };
    crate::save_auth(temp.path(), &previous_auth, AuthCredentialsStoreMode::File)
        .expect("save previous auth");
    let new_auth = crate::AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-new".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
    };

    let err = commit_active_account(
        temp.path(),
        "missing",
        &new_auth,
        AuthCredentialsStoreMode::File,
    )
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
        crate::load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File)
            .expect("read auth json")
            .expect("auth json should exist")
    );
}

#[test]
fn commit_active_account_rollback_without_previous_auth_preserves_stored_accounts() {
    let temp = TempDir::new().expect("tempdir");
    let stored = upsert_api_key_account(
        temp.path(),
        "sk-new".to_string(),
        Some("New".to_string()),
        /*make_active*/ false,
    )
    .expect("upsert api key");
    let auth = crate::AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-new".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
    };

    let err = commit_active_account(
        temp.path(),
        "missing",
        &auth,
        AuthCredentialsStoreMode::File,
    )
    .expect_err("missing account should fail");

    assert_eq!(io::ErrorKind::Other, err.kind());
    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
    assert_eq!(
        None,
        crate::load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File)
            .expect("read auth json")
    );
    assert_eq!(
        vec![stored],
        list_accounts(temp.path()).expect("list accounts")
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
        },
        crate::load_auth_dot_json(temp.path(), AuthCredentialsStoreMode::File)
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
    assert_eq!(
        Some(chatgpt.id.as_str()),
        get_active_account_id(temp.path())
            .expect("active id")
            .as_deref()
    );
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
fn remove_account_matching_credentials_promotes_remaining_account() {
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
fn saved_accounts_file_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");

    upsert_api_key_account(
        temp.path(),
        "sk-test".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("upsert api account");

    let mode = fs::metadata(accounts_file_path(temp.path()))
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(0o600, mode);
}
