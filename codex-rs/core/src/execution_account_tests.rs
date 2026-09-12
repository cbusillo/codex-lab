use super::*;

use crate::config::ConfigBuilder;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn snapshot_auth_provider_fails_closed_after_execution_account_switch() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let initial_auth = CodexAuth::from_external_chatgpt_tokens(
        "header.e30.initial",
        "initial-account",
        /*chatgpt_plan_type*/ None,
    )
    .expect("initial auth");
    let initial_manager =
        AuthManager::from_auth_for_testing_with_home(initial_auth, codex_home.path().to_path_buf());
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("config");
    let lease = ExecutionAccountLease::resolve(
        ThreadId::new(),
        initial_manager,
        ExecutionAccountOptions {
            codex_home: config.codex_home.to_path_buf(),
            auth_home: config.auth_home.to_path_buf(),
            auth_credentials_store_mode: config.cli_auth_credentials_store_mode,
            keyring_backend_kind: config.auth_keyring_backend_kind(),
            forced_chatgpt_workspace_id: config.forced_chatgpt_workspace_id.clone(),
            auth_route_config: config.auth_route_config(),
            chatgpt_base_url: config.chatgpt_base_url.clone(),
            allow_api_key_fallback: false,
            pooling: ExecutionAccountPooling::Disabled,
            persistence: ExecutionAccountLeasePersistence::Ephemeral,
            start: ExecutionAccountStart::New,
        },
    )
    .await;
    let snapshot = lease.snapshot().await;
    assert_eq!(
        snapshot
            .auth_provider
            .to_auth_headers()
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer header.e30.initial")
    );

    let replacement_manager = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_external_chatgpt_tokens(
            "header.e30.replacement",
            "replacement-account",
            /*chatgpt_plan_type*/ None,
        )
        .expect("replacement auth"),
        codex_home.path().to_path_buf(),
    );
    lease.replace_with_detached_auth_manager_for_testing(
        "replacement-account".to_string(),
        replacement_manager,
    );

    assert!(snapshot.auth_provider.to_auth_headers().is_empty());
}
