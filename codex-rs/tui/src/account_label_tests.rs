use codex_login::AccountHealth;
use codex_login::StoredAccount;
use codex_login::token_data::TokenData;
use codex_login::token_data::parse_chatgpt_jwt_claims;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;

use super::account_display_label;
use super::account_mode_priority;

fn stored_account(mode: AuthMode) -> StoredAccount {
    StoredAccount {
        id: "account-id".to_string(),
        mode,
        label: None,
        openai_api_key: None,
        tokens: None,
        last_refresh: None,
        created_at: None,
        last_used_at: None,
        health: AccountHealth::Ok,
    }
}

fn chatgpt_account(email: &str, label: Option<&str>) -> StoredAccount {
    let mut account = stored_account(AuthMode::Chatgpt);
    account.label = label.map(str::to_string);
    account.tokens = Some(TokenData {
        id_token: parse_chatgpt_jwt_claims(&fake_jwt(email)).expect("fake jwt should parse"),
        access_token: "access".to_string(),
        refresh_token: "refresh".to_string(),
        account_id: Some("account-id".to_string()),
    });
    account
}

fn fake_jwt(email: &str) -> String {
    use base64::Engine as _;

    let header = serde_json::json!({ "alg": "none" });
    let payload = serde_json::json!({ "email": email });
    let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    format!(
        "{}.{}.{}",
        encode(&serde_json::to_vec(&header).expect("serialize header")),
        encode(&serde_json::to_vec(&payload).expect("serialize payload")),
        encode(b"sig")
    )
}

#[test]
fn chatgpt_email_label_uses_default_display_format() {
    let account = chatgpt_account("stored@example.com", Some("stored@example.com"));

    assert_eq!(
        account_display_label(&account),
        "ChatGPT (stored@example.com)"
    );
}

#[test]
fn custom_chatgpt_label_is_preserved() {
    let account = chatgpt_account("stored@example.com", Some("Work"));

    assert_eq!(account_display_label(&account), "Work");
}

#[test]
fn api_key_label_masks_secret_suffix_when_label_is_missing() {
    let mut account = stored_account(AuthMode::ApiKey);
    account.openai_api_key = Some("sk-test-secret-value".to_string());

    assert_eq!(account_display_label(&account), "API key (...et-value)");
}

#[test]
fn api_key_label_uses_entire_short_key_as_suffix() {
    let mut account = stored_account(AuthMode::ApiKey);
    account.openai_api_key = Some("sk-short".to_string());

    assert_eq!(account_display_label(&account), "API key (...sk-short)");
}

#[test]
fn whitespace_only_api_key_label_uses_default_display_format() {
    let mut account = stored_account(AuthMode::ApiKey);
    account.label = Some("   ".to_string());
    account.openai_api_key = Some("sk-test-secret-value".to_string());

    assert_eq!(account_display_label(&account), "API key (...et-value)");
}

#[test]
fn custom_api_key_label_is_preserved() {
    let mut account = stored_account(AuthMode::ApiKey);
    account.label = Some("Automation key".to_string());
    account.openai_api_key = Some("sk-test-secret-value".to_string());

    assert_eq!(account_display_label(&account), "Automation key");
}

#[test]
fn account_mode_priority_orders_supported_accounts_first() {
    let priorities = [
        account_mode_priority(AuthMode::Chatgpt),
        account_mode_priority(AuthMode::ChatgptAuthTokens),
        account_mode_priority(AuthMode::ApiKey),
        account_mode_priority(AuthMode::AgentIdentity),
        account_mode_priority(AuthMode::PersonalAccessToken),
    ];

    assert_eq!(priorities, [0, 0, 1, 2, 3]);
}

#[test]
fn unsupported_account_labels_use_mode_defaults() {
    assert_eq!(
        account_display_label(&stored_account(AuthMode::AgentIdentity)),
        "Agent identity"
    );
    assert_eq!(
        account_display_label(&stored_account(AuthMode::PersonalAccessToken)),
        "Personal access token"
    );
}
