use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::ListAccountsResponse;
use codex_app_server_protocol::RemoveAccountResponse;
use codex_app_server_protocol::RemoveAccountStatus;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::SwitchActiveAccountResponse;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use serde::de::DeserializeOwned;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn list_accounts_returns_server_owned_catalog() -> Result<()> {
    let codex_home = TempDir::new()?;
    let first = add_api_key_account(&codex_home, "sk-first", "first")?;
    let second = add_api_key_account(&codex_home, "sk-second", "second")?;
    activate_account(&codex_home, &second.id)?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let response: ListAccountsResponse =
        jsonrpc_response(&mut app_server, "account/list", /*params*/ None).await?;

    assert_eq!(
        response.active_account_id.as_deref(),
        Some(second.id.as_str())
    );
    assert_eq!(response.accounts.len(), 2);
    let first_entry = response
        .accounts
        .iter()
        .find(|entry| entry.account_id == first.id)
        .expect("first account should be listed");
    assert_eq!(first_entry.auth_mode, AuthMode::ApiKey);
    assert_eq!(first_entry.label.as_deref(), Some("first"));
    assert!(!first_entry.is_active);
    let second_entry = response
        .accounts
        .iter()
        .find(|entry| entry.account_id == second.id)
        .expect("second account should be listed");
    assert_eq!(second_entry.auth_mode, AuthMode::ApiKey);
    assert_eq!(second_entry.label.as_deref(), Some("second"));
    assert!(second_entry.is_active);
    Ok(())
}

#[tokio::test]
async fn switch_active_account_materializes_auth_and_notifies() -> Result<()> {
    let codex_home = TempDir::new()?;
    let first = add_api_key_account(&codex_home, "sk-first", "first")?;
    let second = add_api_key_account(&codex_home, "sk-second", "second")?;
    activate_account(&codex_home, &first.id)?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let response: SwitchActiveAccountResponse = jsonrpc_response(
        &mut app_server,
        "account/switchActive",
        Some(json!({ "accountId": second.id })),
    )
    .await?;

    assert_eq!(response.account_id, second.id);
    let notification = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_notification_message("account/updated"),
    )
    .await??;
    let ServerNotification::AccountUpdated(payload) = notification.try_into()? else {
        unreachable!("notification method was filtered to account/updated");
    };
    assert_eq!(payload.auth_mode, Some(AuthMode::ApiKey));
    let auth = codex_login::load_auth_dot_json(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?
    .expect("switch should write auth");
    assert_eq!(auth.openai_api_key.as_deref(), Some("sk-second"));
    Ok(())
}

#[tokio::test]
async fn remove_active_account_promotes_fallback_and_notifies() -> Result<()> {
    let codex_home = TempDir::new()?;
    let fallback = add_api_key_account(&codex_home, "sk-fallback", "fallback")?;
    let active = add_api_key_account(&codex_home, "sk-active", "active")?;
    activate_account(&codex_home, &active.id)?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let response: RemoveAccountResponse = jsonrpc_response(
        &mut app_server,
        "account/remove",
        Some(json!({ "accountId": active.id })),
    )
    .await?;

    assert_eq!(response.status, RemoveAccountStatus::Removed);
    assert_eq!(
        response.active_account_id.as_deref(),
        Some(fallback.id.as_str())
    );
    timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_notification_message("account/updated"),
    )
    .await??;
    let auth = codex_login::load_auth_dot_json(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?
    .expect("fallback should be active");
    assert_eq!(auth.openai_api_key.as_deref(), Some("sk-fallback"));
    Ok(())
}

#[tokio::test]
async fn remove_unknown_account_preserves_active_account() -> Result<()> {
    let codex_home = TempDir::new()?;
    let active = add_api_key_account(&codex_home, "sk-active", "active")?;
    activate_account(&codex_home, &active.id)?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let response: RemoveAccountResponse = jsonrpc_response(
        &mut app_server,
        "account/remove",
        Some(json!({ "accountId": "missing" })),
    )
    .await?;

    assert_eq!(response.status, RemoveAccountStatus::NotFound);
    assert_eq!(
        response.active_account_id.as_deref(),
        Some(active.id.as_str())
    );
    let auth = codex_login::load_auth_dot_json(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?
    .expect("active account should remain materialized");
    assert_eq!(auth.openai_api_key.as_deref(), Some("sk-active"));
    Ok(())
}

#[tokio::test]
async fn remove_only_account_clears_active_auth_and_notifies() -> Result<()> {
    let codex_home = TempDir::new()?;
    let active = add_api_key_account(&codex_home, "sk-active", "active")?;
    activate_account(&codex_home, &active.id)?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let response: RemoveAccountResponse = jsonrpc_response(
        &mut app_server,
        "account/remove",
        Some(json!({ "accountId": active.id })),
    )
    .await?;

    assert_eq!(response.status, RemoveAccountStatus::Removed);
    assert_eq!(response.active_account_id, None);
    let notification = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_notification_message("account/updated"),
    )
    .await??;
    let ServerNotification::AccountUpdated(payload) = notification.try_into()? else {
        unreachable!("notification method was filtered to account/updated");
    };
    assert_eq!(payload.auth_mode, None);
    assert_eq!(
        codex_login::load_auth_dot_json(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn switch_unknown_account_returns_error_and_preserves_active_account() -> Result<()> {
    let codex_home = TempDir::new()?;
    let active = add_api_key_account(&codex_home, "sk-active", "active")?;
    activate_account(&codex_home, &active.id)?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let error = jsonrpc_error(
        &mut app_server,
        "account/switchActive",
        Some(json!({ "accountId": "missing" })),
    )
    .await?;

    assert_eq!(error.error.message, "stored account not found: missing");
    let auth = codex_login::load_auth_dot_json(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?
    .expect("failed switch should preserve active auth");
    assert_eq!(auth.openai_api_key.as_deref(), Some("sk-active"));
    Ok(())
}

#[tokio::test]
async fn switch_rejects_account_disallowed_by_forced_login_method() -> Result<()> {
    let codex_home = TempDir::new()?;
    let account = add_api_key_account(&codex_home, "sk-api", "api")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        "forced_login_method = \"chatgpt\"\n",
    )?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let error = jsonrpc_error(
        &mut app_server,
        "account/switchActive",
        Some(json!({ "accountId": account.id })),
    )
    .await?;

    assert_eq!(
        error.error.message,
        "Stored account activation is disabled. Use a ChatGPT account instead."
    );
    assert_eq!(
        codex_login::load_auth_dot_json(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )?,
        None
    );
    Ok(())
}

fn add_api_key_account(
    codex_home: &TempDir,
    api_key: &str,
    label: &str,
) -> Result<codex_login::StoredAccount> {
    Ok(codex_login::upsert_api_key_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        api_key.to_string(),
        Some(label.to_string()),
        /*make_active*/ false,
    )?)
}

fn activate_account(codex_home: &TempDir, account_id: &str) -> Result<()> {
    codex_login::activate_account(
        codex_home.path(),
        account_id,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    Ok(())
}

async fn initialized_app_server(codex_home: &TempDir) -> Result<TestAppServer> {
    let config_path = codex_home.path().join("config.toml");
    if !config_path.is_file() {
        std::fs::write(config_path, "")?;
    }
    TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[
            ("OPENAI_API_KEY", None),
            ("CODEX_API_KEY", None),
            ("CODEX_ACCESS_TOKEN", None),
        ])
        .build_initialized_with_timeout(DEFAULT_TIMEOUT)
        .await
}

async fn jsonrpc_response<T: DeserializeOwned>(
    app_server: &mut TestAppServer,
    method: &str,
    params: Option<serde_json::Value>,
) -> Result<T> {
    let request_id = app_server.send_raw_request(method, params).await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(response)
}

async fn jsonrpc_error(
    app_server: &mut TestAppServer,
    method: &str,
    params: Option<serde_json::Value>,
) -> Result<JSONRPCError> {
    let request_id = app_server.send_raw_request(method, params).await?;
    timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await?
}
