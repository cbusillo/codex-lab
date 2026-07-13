use super::*;
use crate::auth::AuthDotJson;
use crate::auth::save_auth;
use crate::auth_accounts::StoredAccount;
use crate::auth_profiles::profile_home;
use crate::auth_profiles::record_auth_profile_login;
use crate::token_data::IdTokenInfo;
use crate::token_data::TokenData;
use base64::Engine;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;
use serde::Serialize;
use std::fs;
use std::io;
use std::path::Path;
use tempfile::TempDir;

const TEST_AUTH_CREDENTIALS_STORE_MODE: AuthCredentialsStoreMode = AuthCredentialsStoreMode::File;

fn import_auth_accounts_from_auth_homes(codex_home: &Path) -> io::Result<AuthAccountImportReport> {
    super::import_auth_accounts_from_auth_homes(codex_home, TEST_AUTH_CREDENTIALS_STORE_MODE)
}

fn list_accounts(codex_home: &Path) -> io::Result<Vec<StoredAccount>> {
    crate::auth_accounts::list_accounts(codex_home, TEST_AUTH_CREDENTIALS_STORE_MODE)
}

fn get_active_account_id(codex_home: &Path) -> io::Result<Option<String>> {
    crate::auth_accounts::get_active_account_id(codex_home, TEST_AUTH_CREDENTIALS_STORE_MODE)
}

fn make_chatgpt_tokens(account_id: &str, email: &str) -> TokenData {
    fn fake_jwt(account_id: &str, email: &str) -> String {
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }

        let payload = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id,
                "chatgpt_user_id": "user-12345",
                "user_id": "user-12345"
            }
        });
        let b64 = |value: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(value).expect("json to vec"))
        };
        let header_b64 = b64(&serde_json::to_value(Header {
            alg: "none",
            typ: "JWT",
        })
        .expect("header value"));
        let payload_b64 = b64(&payload);
        let signature_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"sig");
        format!("{header_b64}.{payload_b64}.{signature_b64}")
    }

    TokenData {
        id_token: IdTokenInfo {
            email: Some(email.to_string()),
            chatgpt_plan_type: None,
            chatgpt_user_id: Some("user-12345".to_string()),
            chatgpt_account_id: Some(account_id.to_string()),
            chatgpt_account_is_fedramp: false,
            raw_jwt: fake_jwt(account_id, email),
        },
        access_token: format!("access-{account_id}"),
        refresh_token: format!("refresh-{account_id}"),
        account_id: Some(account_id.to_string()),
    }
}

fn save_api_key_auth(codex_home: &std::path::Path, api_key: &str) {
    save_auth(
        codex_home,
        &AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: Some(api_key.to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save api key auth");
}

fn save_chatgpt_auth(codex_home: &std::path::Path, account_id: &str, email: &str) {
    save_auth(
        codex_home,
        &AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(make_chatgpt_tokens(account_id, email)),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save chatgpt auth");
}

fn save_pat_auth(codex_home: &std::path::Path) {
    save_auth(
        codex_home,
        &AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: Some("pat-token".to_string()),
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save pat auth");
}

#[test]
fn import_empty_home_reports_missing_default_auth() {
    let temp = TempDir::new().expect("tempdir");

    let report = import_auth_accounts_from_auth_homes(temp.path()).expect("import accounts");

    assert_eq!(
        AuthAccountImportReport {
            imported: Vec::new(),
            skipped: vec![SkippedAuthAccountImport {
                source: AuthAccountImportSource::Default,
                reason: AuthAccountImportSkipReason::MissingAuth,
            }],
        },
        report
    );
    assert_eq!(
        Vec::<crate::auth_accounts::StoredAccount>::new(),
        list_accounts(temp.path()).expect("list accounts")
    );
}

#[test]
fn imports_default_and_named_profile_auth_into_root_account_store() {
    let temp = TempDir::new().expect("tempdir");
    save_api_key_auth(temp.path(), "sk-default");
    let work_home = profile_home(temp.path(), "work").expect("profile home");
    record_auth_profile_login(
        temp.path(),
        "work",
        /*account_id*/ None,
        /*email*/ None,
    )
    .expect("record profile");
    save_chatgpt_auth(&work_home, "acct-work", "work@example.com");

    let report = import_auth_accounts_from_auth_homes(temp.path()).expect("import accounts");
    let accounts = list_accounts(temp.path()).expect("list accounts");

    assert_eq!(2, report.imported.len());
    assert_eq!(0, report.skipped.len());
    assert_eq!(2, accounts.len());
    assert!(accounts.iter().any(|account| {
        account.mode == AuthMode::ApiKey && account.openai_api_key.as_deref() == Some("sk-default")
    }));
    assert!(accounts.iter().any(|account| {
        account.mode == AuthMode::Chatgpt
            && account
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.account_id.as_deref())
                == Some("acct-work")
    }));
    assert_eq!(None, get_active_account_id(temp.path()).expect("active id"));
}

#[test]
fn import_is_idempotent_and_does_not_overwrite_existing_accounts() {
    let temp = TempDir::new().expect("tempdir");
    save_api_key_auth(temp.path(), "sk-default");

    let first = import_auth_accounts_from_auth_homes(temp.path()).expect("first import");
    let first_accounts = list_accounts(temp.path()).expect("first accounts");
    let second = import_auth_accounts_from_auth_homes(temp.path()).expect("second import");
    let second_accounts = list_accounts(temp.path()).expect("second accounts");

    assert_eq!(1, first.imported.len());
    assert_eq!(
        AuthAccountImportReport {
            imported: Vec::new(),
            skipped: vec![SkippedAuthAccountImport {
                source: AuthAccountImportSource::Default,
                reason: AuthAccountImportSkipReason::ExistingAccount,
            }],
        },
        second
    );
    assert_eq!(first_accounts, second_accounts);
}

#[test]
fn import_dedupes_profiles_by_api_key_identity_not_profile_name() {
    let temp = TempDir::new().expect("tempdir");
    for profile in ["work", "backup"] {
        let home = profile_home(temp.path(), profile).expect("profile home");
        record_auth_profile_login(
            temp.path(),
            profile,
            /*account_id*/ None,
            /*email*/ None,
        )
        .expect("record profile");
        save_api_key_auth(&home, "sk-shared");
    }

    let report = import_auth_accounts_from_auth_homes(temp.path()).expect("import accounts");

    assert_eq!(
        AuthAccountImportReport {
            imported: vec![ImportedAuthAccount {
                source: AuthAccountImportSource::Profile {
                    name: "backup".to_string(),
                },
                account: report.imported[0].account.clone(),
            }],
            skipped: vec![
                SkippedAuthAccountImport {
                    source: AuthAccountImportSource::Default,
                    reason: AuthAccountImportSkipReason::MissingAuth,
                },
                SkippedAuthAccountImport {
                    source: AuthAccountImportSource::Profile {
                        name: "work".to_string(),
                    },
                    reason: AuthAccountImportSkipReason::ExistingAccount,
                },
            ],
        },
        report
    );
    assert_eq!(1, list_accounts(temp.path()).expect("list accounts").len());
}

#[test]
fn import_dedupes_chatgpt_by_account_identity_not_profile_name() {
    let temp = TempDir::new().expect("tempdir");
    for (profile, email) in [("work", "WORK@example.com"), ("backup", "work@example.com")] {
        let home = profile_home(temp.path(), profile).expect("profile home");
        record_auth_profile_login(
            temp.path(),
            profile,
            /*account_id*/ None,
            /*email*/ None,
        )
        .expect("record profile");
        save_chatgpt_auth(&home, "acct-shared", email);
    }

    let report = import_auth_accounts_from_auth_homes(temp.path()).expect("import accounts");

    assert_eq!(
        AuthAccountImportReport {
            imported: vec![ImportedAuthAccount {
                source: AuthAccountImportSource::Profile {
                    name: "backup".to_string(),
                },
                account: report.imported[0].account.clone(),
            }],
            skipped: vec![
                SkippedAuthAccountImport {
                    source: AuthAccountImportSource::Default,
                    reason: AuthAccountImportSkipReason::MissingAuth,
                },
                SkippedAuthAccountImport {
                    source: AuthAccountImportSource::Profile {
                        name: "work".to_string(),
                    },
                    reason: AuthAccountImportSkipReason::ExistingAccount,
                },
            ],
        },
        report
    );
    assert_eq!(1, list_accounts(temp.path()).expect("list accounts").len());
}

#[test]
fn import_keeps_same_email_different_chatgpt_accounts_distinct() {
    let temp = TempDir::new().expect("tempdir");
    save_chatgpt_auth(temp.path(), "acct-personal", "user@example.com");
    let work_home = profile_home(temp.path(), "work").expect("profile home");
    record_auth_profile_login(
        temp.path(),
        "work",
        /*account_id*/ None,
        /*email*/ None,
    )
    .expect("record profile");
    save_chatgpt_auth(&work_home, "acct-work", "user@example.com");

    let report = import_auth_accounts_from_auth_homes(temp.path()).expect("import accounts");

    assert_eq!(2, report.imported.len());
    assert_eq!(2, list_accounts(temp.path()).expect("list accounts").len());
}

#[test]
fn import_preserves_existing_active_account() {
    let temp = TempDir::new().expect("tempdir");
    let active = crate::auth_accounts::upsert_api_key_account(
        temp.path(),
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        "sk-active".to_string(),
        /*label*/ None,
        /*make_active*/ true,
    )
    .expect("insert active account");
    crate::auth_accounts::set_active_account_id(
        temp.path(),
        TEST_AUTH_CREDENTIALS_STORE_MODE,
        Some(active.id.clone()),
    )
    .expect("set active");
    save_api_key_auth(temp.path(), "sk-imported");

    import_auth_accounts_from_auth_homes(temp.path()).expect("import accounts");

    assert_eq!(
        Some(active.id),
        get_active_account_id(temp.path()).expect("active account id")
    );
}

#[test]
fn import_skips_missing_invalid_and_unsupported_auth_sources() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(temp.path().join("auth.json"), "not json").expect("write invalid auth");

    record_auth_profile_login(
        temp.path(),
        "missing",
        /*account_id*/ None,
        /*email*/ None,
    )
    .expect("record missing profile");
    let pat_home = profile_home(temp.path(), "pat").expect("pat profile home");
    record_auth_profile_login(
        temp.path(),
        "pat",
        /*account_id*/ None,
        /*email*/ None,
    )
    .expect("record pat profile");
    save_pat_auth(&pat_home);

    let report = import_auth_accounts_from_auth_homes(temp.path()).expect("import accounts");

    assert_eq!(
        AuthAccountImportReport {
            imported: Vec::new(),
            skipped: vec![
                SkippedAuthAccountImport {
                    source: AuthAccountImportSource::Default,
                    reason: AuthAccountImportSkipReason::InvalidAuth,
                },
                SkippedAuthAccountImport {
                    source: AuthAccountImportSource::Profile {
                        name: "missing".to_string(),
                    },
                    reason: AuthAccountImportSkipReason::MissingAuth,
                },
                SkippedAuthAccountImport {
                    source: AuthAccountImportSource::Profile {
                        name: "pat".to_string(),
                    },
                    reason: AuthAccountImportSkipReason::UnsupportedMode,
                },
            ],
        },
        report
    );
}

#[test]
fn import_reports_parseable_auth_missing_credentials_as_invalid() {
    let temp = TempDir::new().expect("tempdir");
    save_auth(
        temp.path(),
        &AuthDotJson {
            auth_mode: Some(AuthMode::ApiKey),
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save api key auth without key");

    let chatgpt_home = profile_home(temp.path(), "chatgpt").expect("chatgpt profile home");
    record_auth_profile_login(
        temp.path(),
        "chatgpt",
        /*account_id*/ None,
        /*email*/ None,
    )
    .expect("record chatgpt profile");
    save_auth(
        &chatgpt_home,
        &AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
        },
        AuthCredentialsStoreMode::File,
    )
    .expect("save chatgpt auth without tokens");

    let report = import_auth_accounts_from_auth_homes(temp.path()).expect("import accounts");

    assert_eq!(
        AuthAccountImportReport {
            imported: Vec::new(),
            skipped: vec![
                SkippedAuthAccountImport {
                    source: AuthAccountImportSource::Default,
                    reason: AuthAccountImportSkipReason::InvalidAuth,
                },
                SkippedAuthAccountImport {
                    source: AuthAccountImportSource::Profile {
                        name: "chatgpt".to_string(),
                    },
                    reason: AuthAccountImportSkipReason::InvalidAuth,
                },
            ],
        },
        report
    );
    assert_eq!(
        Vec::<crate::auth_accounts::StoredAccount>::new(),
        list_accounts(temp.path()).expect("list accounts")
    );
}
