use super::*;
use crate::app::test_support::make_test_app;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;

#[test]
fn current_account_entry_fills_missing_catalog_identity() {
    assert_eq!(
        current_account_list_entry(Account::ApiKey {}, /*account_id*/ None),
        AccountListEntry {
            account_id: "current".to_string(),
            auth_mode: AuthMode::ApiKey,
            health: AccountHealth::Ok,
            label: Some("API key".to_string()),
            created_at: None,
            last_used_at: None,
            is_active: true,
        }
    );
}

#[tokio::test]
async fn login_storage_routing_keeps_remote_mutations_on_the_server() {
    let mut app = make_test_app().await;
    app.config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::Keyring;

    assert!(!app.use_default_store_login());
    assert!(!app.preserve_existing_app_server_account());

    app.app_server_target = crate::AppServerTarget::Remote {
        endpoint: crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url: "ws://127.0.0.1:4500".to_string(),
            auth_token: None,
        },
    };

    assert!(!app.use_default_store_login());
    assert!(app.preserve_existing_app_server_account());
}

#[tokio::test]
async fn api_key_login_saves_trimmed_key_to_default_store() {
    let mut app = make_test_app().await;
    let codex_home = tempfile::tempdir().expect("create temporary Codex home");
    app.config.codex_home = codex_home
        .path()
        .to_path_buf()
        .try_into()
        .expect("temporary Codex home should be absolute");
    app.config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;

    app.save_default_store_login_add_account_api_key("sk-test-direct-login");

    let stored = codex_login::load_auth_dot_json(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        app.config.auth_keyring_backend_kind(),
    )
    .expect("load stored auth")
    .expect("API key login should write auth");
    assert_eq!(
        stored.openai_api_key.as_deref(),
        Some("sk-test-direct-login")
    );
}

#[tokio::test]
async fn cancelling_direct_device_code_login_clears_and_signals_pending_attempt() {
    let mut app = make_test_app().await;
    let cancellation = CancellationToken::new();
    app.pending_direct_login_add_account = Some(PendingDirectLoginAddAccount {
        attempt_id: 1,
        cancellation: PendingDirectLoginAddAccountCancellation::DeviceCode(cancellation.clone()),
    });

    app.cancel_direct_login_add_account();

    assert!(app.pending_direct_login_add_account.is_none());
    assert!(cancellation.is_cancelled());
}

#[tokio::test]
async fn dropping_app_cancels_pending_direct_device_code_login() {
    let cancellation = CancellationToken::new();
    {
        let mut app = make_test_app().await;
        app.pending_direct_login_add_account = Some(PendingDirectLoginAddAccount {
            attempt_id: 1,
            cancellation: PendingDirectLoginAddAccountCancellation::DeviceCode(
                cancellation.clone(),
            ),
        });
    }

    assert!(cancellation.is_cancelled());
}

#[tokio::test]
async fn stale_direct_login_completion_does_not_clear_current_attempt() {
    let mut app = make_test_app().await;
    let cancellation = CancellationToken::new();
    app.pending_direct_login_add_account = Some(PendingDirectLoginAddAccount {
        attempt_id: 2,
        cancellation: PendingDirectLoginAddAccountCancellation::DeviceCode(cancellation.clone()),
    });

    app.complete_login_add_account_chatgpt(/*attempt_id*/ 1, Ok(()));

    assert_eq!(
        app.pending_direct_login_add_account
            .as_ref()
            .map(|pending| pending.attempt_id),
        Some(2)
    );
    assert!(!cancellation.is_cancelled());
}
