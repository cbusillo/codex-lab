use super::*;

use std::path::Path;

use base64::Engine;
use chrono::Duration;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::auth::save_auth;
use codex_login::token_data::IdTokenInfo;
use codex_login::token_data::TokenData;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use pretty_assertions::assert_eq;

fn chatgpt_tokens(account_id: &str) -> TokenData {
    let header = serde_json::json!({"alg": "none", "typ": "JWT"});
    let payload = serde_json::json!({
        "email": format!("{account_id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": format!("user-{account_id}"),
            "user_id": format!("user-{account_id}")
        }
    });
    let encode = |value: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(value).expect("serialize jwt segment"))
    };
    TokenData {
        id_token: IdTokenInfo {
            email: Some(format!("{account_id}@example.com")),
            chatgpt_plan_type: None,
            chatgpt_user_id: Some(format!("user-{account_id}")),
            chatgpt_account_id: Some(account_id.to_string()),
            chatgpt_account_is_fedramp: false,
            raw_jwt: format!("{}.{}.signature", encode(&header), encode(&payload)),
        },
        access_token: format!("access-{account_id}"),
        refresh_token: format!("refresh-{account_id}"),
        account_id: Some(account_id.to_string()),
    }
}

fn rate_limit_snapshot(resets_at: DateTime<Utc>, used_percent: f64) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: Some("Codex".to_string()),
        primary: Some(RateLimitWindow {
            used_percent,
            window_minutes: Some(300),
            resets_at: Some(resets_at.timestamp()),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    }
}

async fn test_accounts() -> (
    tempfile::TempDir,
    Arc<AuthManager>,
    StoredAccount,
    StoredAccount,
) {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let control = codex_login::upsert_chatgpt_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        chatgpt_tokens("control"),
        Utc::now(),
        Some("Control".to_string()),
        /*make_active*/ true,
    )
    .expect("store control account");
    let execution = codex_login::upsert_chatgpt_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        chatgpt_tokens("execution"),
        Utc::now(),
        Some("Execution".to_string()),
        /*make_active*/ false,
    )
    .expect("store execution account");
    let (_account, control_auth) = codex_login::auth_for_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        &control.id,
    )
    .expect("control auth");
    save_auth(
        codex_home.path(),
        &control_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save control auth");
    let control_manager = Arc::new(
        AuthManager::new(
            codex_home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
                OutboundProxyPolicy::ReqwestDefault,
            )),
        )
        .await,
    );
    (codex_home, control_manager, control, execution)
}

fn options(
    codex_home: &Path,
    pooling: ExecutionAccountPooling,
    start: ExecutionAccountStart,
) -> ExecutionAccountOptions {
    ExecutionAccountOptions {
        codex_home: codex_home.to_path_buf(),
        auth_home: codex_home.to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::default(),
        forced_chatgpt_workspace_id: None,
        auth_route_config: AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
            OutboundProxyPolicy::ReqwestDefault,
        )),
        chatgpt_base_url: "https://chatgpt.com/backend-api/".to_string(),
        allow_api_key_fallback: false,
        pooling,
        persistence: ExecutionAccountLeasePersistence::Durable,
        start,
    }
}

fn prefer_execution_account(
    codex_home: &Path,
    control: &StoredAccount,
    execution: &StoredAccount,
    now: DateTime<Utc>,
) {
    crate::account_usage::record_rate_limit_snapshot(
        codex_home,
        &control.id,
        rate_limit_snapshot(now + Duration::hours(4), 40.0),
        now,
    )
    .expect("record control usage");
    crate::account_usage::record_rate_limit_snapshot(
        codex_home,
        &execution.id,
        rate_limit_snapshot(now + Duration::hours(1), 40.0),
        now,
    )
    .expect("record execution usage");
}

#[tokio::test]
async fn lease_prefers_reset_soonest_and_stays_pinned_without_changing_control() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    let now = Utc::now();
    prefer_execution_account(codex_home.path(), &control, &execution, now);
    let thread_id = ThreadId::default();

    let first = ExecutionAccountLease::resolve(
        thread_id,
        Arc::clone(&control_manager),
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::New,
        ),
    )
    .await;
    assert_eq!(
        first.identity().stored_account_id,
        Some(execution.id.clone())
    );
    assert_eq!(
        first.prompt_cache_discriminator(),
        Some(execution.id.clone())
    );
    assert_eq!(
        read_persisted_lease(codex_home.path(), thread_id),
        Some(execution.id.clone())
    );
    assert_eq!(
        codex_login::get_active_account_id(codex_home.path(), AuthCredentialsStoreMode::File)
            .expect("active account"),
        Some(control.id.clone())
    );
    assert_eq!(
        control_manager
            .auth_cached()
            .and_then(|auth| auth.get_account_id()),
        Some("control".to_string())
    );

    crate::account_usage::record_rate_limit_snapshot(
        codex_home.path(),
        &control.id,
        rate_limit_snapshot(now + Duration::minutes(10), 1.0),
        now,
    )
    .expect("update control usage");
    let resumed = ExecutionAccountLease::resolve(
        thread_id,
        Arc::clone(&control_manager),
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::Resumed,
        ),
    )
    .await;
    assert_eq!(
        resumed.identity().stored_account_id,
        Some(execution.id.clone())
    );
    assert_eq!(
        resumed.prompt_cache_discriminator(),
        Some(execution.id.clone())
    );
}

#[tokio::test]
async fn ephemeral_lease_reads_source_lease_but_never_persists_a_destination_record() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    prefer_execution_account(codex_home.path(), &control, &execution, Utc::now());
    let source_thread_id = ThreadId::new();
    persist_lease(codex_home.path(), source_thread_id, &execution.id)
        .expect("persist source lease");
    let forked_thread_id = ThreadId::new();

    let mut resolve_options = options(
        codex_home.path(),
        ExecutionAccountPooling::Enabled,
        ExecutionAccountStart::Forked {
            source_thread_id: Some(source_thread_id),
        },
    );
    resolve_options.persistence = ExecutionAccountLeasePersistence::Ephemeral;
    let lease =
        ExecutionAccountLease::resolve(forked_thread_id, control_manager, resolve_options).await;

    // The ephemeral fork still inherits the durable source lease's account...
    assert_eq!(
        lease.identity().stored_account_id,
        Some(execution.id.clone())
    );
    assert_eq!(lease.prompt_cache_discriminator(), Some(execution.id));
    // ...but must not write a destination lease record of its own.
    assert_eq!(
        read_persisted_lease(codex_home.path(), forked_thread_id),
        None
    );
    assert!(!lease_path(codex_home.path(), forked_thread_id).exists());
}

#[tokio::test]
async fn ephemeral_lease_failover_does_not_persist_a_destination_record() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    let now = Utc::now();
    prefer_execution_account(codex_home.path(), &control, &execution, now);
    let thread_id = ThreadId::new();

    let mut resolve_options = options(
        codex_home.path(),
        ExecutionAccountPooling::Enabled,
        ExecutionAccountStart::New,
    );
    resolve_options.persistence = ExecutionAccountLeasePersistence::Ephemeral;
    let lease = ExecutionAccountLease::resolve(thread_id, control_manager, resolve_options).await;
    assert_eq!(
        lease.identity().stored_account_id,
        Some(execution.id.clone())
    );
    assert!(!lease_path(codex_home.path(), thread_id).exists());

    let switched = lease
        .failover_after_usage_limit(
            &mut RateLimitSwitchState::default(),
            Some(now + Duration::hours(1)),
        )
        .await
        .expect("fail over")
        .expect("replacement account");
    assert_eq!(switched.stored_account_id, Some(control.id));
    assert!(!lease_path(codex_home.path(), thread_id).exists());
}

#[tokio::test]
async fn cleared_thread_selects_and_persists_preferred_account() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    prefer_execution_account(codex_home.path(), &control, &execution, Utc::now());
    let thread_id = ThreadId::new();

    let lease = ExecutionAccountLease::resolve(
        thread_id,
        control_manager,
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::Cleared,
        ),
    )
    .await;

    assert_eq!(
        lease.identity().stored_account_id,
        Some(execution.id.clone())
    );
    assert_eq!(
        read_persisted_lease(codex_home.path(), thread_id),
        Some(execution.id)
    );
}

#[tokio::test]
async fn resumed_thread_without_lease_binds_and_persists_current_control() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    prefer_execution_account(codex_home.path(), &control, &execution, Utc::now());
    let thread_id = ThreadId::new();

    let lease = ExecutionAccountLease::resolve(
        thread_id,
        Arc::clone(&control_manager),
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::Resumed,
        ),
    )
    .await;

    assert_eq!(lease.identity().stored_account_id, Some(control.id.clone()));
    assert!(Arc::ptr_eq(&lease.auth_manager(), &control_manager));
    assert_eq!(lease.prompt_cache_discriminator(), None);
    assert_eq!(
        read_persisted_lease(codex_home.path(), thread_id),
        Some(control.id)
    );
}

#[tokio::test]
async fn fork_inherits_source_lease_or_binds_current_control() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    prefer_execution_account(codex_home.path(), &control, &execution, Utc::now());
    let source_thread_id = ThreadId::new();
    persist_lease(codex_home.path(), source_thread_id, &execution.id)
        .expect("persist source lease");
    let inherited_thread_id = ThreadId::new();

    let inherited = ExecutionAccountLease::resolve(
        inherited_thread_id,
        Arc::clone(&control_manager),
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::Forked {
                source_thread_id: Some(source_thread_id),
            },
        ),
    )
    .await;
    assert_eq!(
        inherited.identity().stored_account_id,
        Some(execution.id.clone())
    );
    assert_eq!(
        inherited.prompt_cache_discriminator(),
        Some(execution.id.clone())
    );
    assert_eq!(
        read_persisted_lease(codex_home.path(), inherited_thread_id),
        Some(execution.id)
    );

    let missing_source_thread_id = ThreadId::new();
    let fallback_thread_id = ThreadId::new();
    let fallback = ExecutionAccountLease::resolve(
        fallback_thread_id,
        Arc::clone(&control_manager),
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::Forked {
                source_thread_id: Some(missing_source_thread_id),
            },
        ),
    )
    .await;
    assert_eq!(
        fallback.identity().stored_account_id,
        Some(control.id.clone())
    );
    assert!(Arc::ptr_eq(&fallback.auth_manager(), &control_manager));
    assert_eq!(
        read_persisted_lease(codex_home.path(), fallback_thread_id),
        Some(control.id)
    );
}

#[tokio::test]
async fn invalid_or_corrupt_resumed_lease_falls_back_to_control_and_is_repaired() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    prefer_execution_account(codex_home.path(), &control, &execution, Utc::now());
    let corrupt_thread_id = ThreadId::new();
    let corrupt_path = lease_path(codex_home.path(), corrupt_thread_id);
    std::fs::create_dir_all(corrupt_path.parent().expect("lease parent"))
        .expect("create lease directory");
    std::fs::write(corrupt_path, b"{not-json").expect("write corrupt lease");
    let invalid_thread_id = ThreadId::new();
    persist_lease(codex_home.path(), invalid_thread_id, "missing-account")
        .expect("persist invalid lease");

    for thread_id in [corrupt_thread_id, invalid_thread_id] {
        let lease = ExecutionAccountLease::resolve(
            thread_id,
            Arc::clone(&control_manager),
            options(
                codex_home.path(),
                ExecutionAccountPooling::Enabled,
                ExecutionAccountStart::Resumed,
            ),
        )
        .await;
        assert_eq!(lease.identity().stored_account_id, Some(control.id.clone()));
        assert!(Arc::ptr_eq(&lease.auth_manager(), &control_manager));
        assert_eq!(
            read_persisted_lease(codex_home.path(), thread_id),
            Some(control.id.clone())
        );
    }
}

#[tokio::test]
async fn pooled_execution_disabled_stays_on_control_and_persists_cache_owner() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    prefer_execution_account(codex_home.path(), &control, &execution, Utc::now());
    let thread_id = ThreadId::new();
    let lease = ExecutionAccountLease::resolve(
        thread_id,
        Arc::clone(&control_manager),
        options(
            codex_home.path(),
            ExecutionAccountPooling::Disabled,
            ExecutionAccountStart::New,
        ),
    )
    .await;

    assert_eq!(lease.identity().stored_account_id, Some(control.id));
    assert!(Arc::ptr_eq(&lease.auth_manager(), &control_manager));
    assert_eq!(
        read_persisted_lease(codex_home.path(), thread_id),
        lease.identity().stored_account_id
    );
    assert_eq!(
        lease
            .failover_after_usage_limit(
                &mut RateLimitSwitchState::default(),
                /*blocked_until*/ None,
            )
            .await
            .expect("pooled-disabled failover"),
        None
    );
}

#[tokio::test]
async fn pooled_execution_requires_control_to_match_active_catalog_account() {
    let (codex_home, _catalog_manager, control, execution) = test_accounts().await;
    prefer_execution_account(codex_home.path(), &control, &execution, Utc::now());
    let non_catalog_manager =
        AuthManager::from_auth_for_testing(CodexAuth::from_api_key("sk-non-catalog"));
    let thread_id = ThreadId::new();

    let lease = ExecutionAccountLease::resolve(
        thread_id,
        Arc::clone(&non_catalog_manager),
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::New,
        ),
    )
    .await;

    assert_eq!(lease.identity().stored_account_id, None);
    assert!(Arc::ptr_eq(&lease.auth_manager(), &non_catalog_manager));
    assert_eq!(lease.prompt_cache_discriminator(), None);
    assert_eq!(read_persisted_lease(codex_home.path(), thread_id), None);
    assert_eq!(
        lease
            .failover_after_usage_limit(
                &mut RateLimitSwitchState::default(),
                /*blocked_until*/ None,
            )
            .await
            .expect("non-catalog failover"),
        None
    );
}

#[tokio::test]
async fn api_key_control_account_does_not_pool_chatgpt_accounts_for_any_start() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let control = codex_login::upsert_api_key_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        "sk-control".to_string(),
        Some("Control API key".to_string()),
        /*make_active*/ true,
    )
    .expect("store control api key account");
    let execution = codex_login::upsert_chatgpt_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        chatgpt_tokens("execution"),
        Utc::now(),
        Some("Execution ChatGPT".to_string()),
        /*make_active*/ false,
    )
    .expect("store execution ChatGPT account");
    let (_account, control_auth) = codex_login::auth_for_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        &control.id,
    )
    .expect("control api key auth");
    save_auth(
        codex_home.path(),
        &control_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save control api key auth");
    let control_manager = Arc::new(
        AuthManager::new(
            codex_home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
                OutboundProxyPolicy::ReqwestDefault,
            )),
        )
        .await,
    );
    assert_eq!(control_manager.auth_mode(), Some(AuthMode::ApiKey));
    assert_eq!(
        codex_login::get_active_account_id(codex_home.path(), AuthCredentialsStoreMode::File)
            .expect("active account"),
        Some(control.id.clone())
    );

    let resumed_thread_id = ThreadId::new();
    persist_lease(codex_home.path(), resumed_thread_id, &execution.id)
        .expect("persist resumed ChatGPT lease");
    let source_thread_id = ThreadId::new();
    persist_lease(codex_home.path(), source_thread_id, &execution.id)
        .expect("persist fork source ChatGPT lease");
    let new_thread_id = ThreadId::new();
    let cleared_thread_id = ThreadId::new();
    let forked_thread_id = ThreadId::new();

    for (thread_id, start, expected_cache_discriminator) in [
        (new_thread_id, ExecutionAccountStart::New, None),
        (cleared_thread_id, ExecutionAccountStart::Cleared, None),
        (
            resumed_thread_id,
            ExecutionAccountStart::Resumed,
            Some(control.id.clone()),
        ),
        (
            forked_thread_id,
            ExecutionAccountStart::Forked {
                source_thread_id: Some(source_thread_id),
            },
            None,
        ),
    ] {
        let mut resolve_options =
            options(codex_home.path(), ExecutionAccountPooling::Enabled, start);
        resolve_options.allow_api_key_fallback = true;
        let lease = ExecutionAccountLease::resolve(
            thread_id,
            Arc::clone(&control_manager),
            resolve_options,
        )
        .await;

        assert_eq!(lease.identity().stored_account_id, Some(control.id.clone()));
        assert_eq!(lease.identity().mode, AuthMode::ApiKey);
        assert!(Arc::ptr_eq(&lease.auth_manager(), &control_manager));
        assert_eq!(
            lease.prompt_cache_discriminator(),
            expected_cache_discriminator
        );
    }

    for thread_id in [
        new_thread_id,
        cleared_thread_id,
        resumed_thread_id,
        forked_thread_id,
    ] {
        assert_eq!(
            read_persisted_lease(codex_home.path(), thread_id),
            Some(control.id.clone())
        );
    }
}

#[tokio::test]
async fn execution_auth_revision_is_strictly_monotonic_across_replacements() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let control_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("sk-control"));
    let lease = ExecutionAccountLease::resolve(
        ThreadId::new(),
        control_manager,
        options(
            codex_home.path(),
            ExecutionAccountPooling::Disabled,
            ExecutionAccountStart::New,
        ),
    )
    .await;
    let initial_revision = lease.auth_revision();

    lease.replace_with_detached_auth_manager_for_testing(
        "first".to_string(),
        AuthManager::from_auth_for_testing(CodexAuth::from_api_key("sk-first")),
    );
    let first_replacement_revision = lease.auth_revision();
    lease.replace_with_detached_auth_manager_for_testing(
        "second".to_string(),
        AuthManager::from_auth_for_testing(CodexAuth::from_api_key("sk-second")),
    );
    let second_replacement_revision = lease.auth_revision();

    assert!(initial_revision < first_replacement_revision);
    assert!(first_replacement_revision < second_replacement_revision);
}

#[test]
fn persist_lease_atomically_replaces_existing_lease() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let thread_id = ThreadId::new();

    persist_lease(codex_home.path(), thread_id, "first").expect("persist first lease");
    persist_lease(codex_home.path(), thread_id, "second").expect("replace lease");

    assert_eq!(
        read_persisted_lease(codex_home.path(), thread_id),
        Some("second".to_string())
    );
}

#[tokio::test]
async fn usage_limit_failover_changes_only_execution_lease() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    let now = Utc::now();
    prefer_execution_account(codex_home.path(), &control, &execution, now);
    let lease = ExecutionAccountLease::resolve(
        ThreadId::default(),
        Arc::clone(&control_manager),
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::New,
        ),
    )
    .await;
    let previous_revision = lease.auth_revision();
    let switched = lease
        .failover_after_usage_limit(
            &mut RateLimitSwitchState::default(),
            Some(now + Duration::hours(1)),
        )
        .await
        .expect("fail over")
        .expect("replacement account");

    assert_eq!(switched.stored_account_id, Some(control.id.clone()));
    assert!(lease.auth_revision() > previous_revision);
    assert_eq!(lease.prompt_cache_discriminator(), None);
    assert_eq!(
        codex_login::get_active_account_id(codex_home.path(), AuthCredentialsStoreMode::File)
            .expect("active account"),
        Some(control.id)
    );
    assert_eq!(
        control_manager
            .auth_cached()
            .and_then(|auth| auth.get_account_id()),
        Some("control".to_string())
    );
}

#[tokio::test]
async fn pooled_execution_lease_shares_then_detaches_from_control_account() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    let thread_id = ThreadId::default();
    persist_lease(codex_home.path(), thread_id, &control.id).expect("persist control lease");
    let lease = ExecutionAccountLease::resolve(
        thread_id,
        Arc::clone(&control_manager),
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::Resumed,
        ),
    )
    .await;
    assert_eq!(lease.identity().stored_account_id, Some(control.id.clone()));
    assert!(Arc::ptr_eq(&lease.auth_manager(), &control_manager));
    assert_eq!(lease.prompt_cache_discriminator(), None);

    let (_account, execution_auth) = codex_login::auth_for_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        &execution.id,
    )
    .expect("execution auth");
    save_auth(
        codex_home.path(),
        &execution_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save switched control auth");
    codex_login::set_active_account_id(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        Some(execution.id.clone()),
    )
    .expect("switch control account");
    assert_eq!(
        lease
            .failover_after_usage_limit(
                &mut RateLimitSwitchState::default(),
                /*blocked_until*/ None,
            )
            .await
            .expect("mismatched-control failover"),
        None
    );

    lease.prepare_for_control_auth_reload().await;
    assert!(!Arc::ptr_eq(&lease.auth_manager(), &control_manager));
    assert_eq!(lease.prompt_cache_discriminator(), None);
    control_manager.reload().await;
    lease.reconcile_after_control_auth_reload().await;

    assert_eq!(
        control_manager
            .auth_cached()
            .and_then(|auth| auth.get_account_id()),
        Some("execution".to_string())
    );
    assert_eq!(lease.identity().stored_account_id, Some(control.id.clone()));
    assert_eq!(
        lease
            .auth_manager()
            .auth_cached()
            .and_then(|auth| auth.get_account_id()),
        Some("control".to_string())
    );

    let (_account, control_auth) = codex_login::auth_for_account(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        &control.id,
    )
    .expect("control auth");
    save_auth(
        codex_home.path(),
        &control_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("restore control auth");
    codex_login::set_active_account_id(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        Some(control.id.clone()),
    )
    .expect("restore control account");

    lease.prepare_for_control_auth_reload().await;
    control_manager.reload().await;
    lease.reconcile_after_control_auth_reload().await;

    assert!(Arc::ptr_eq(&lease.auth_manager(), &control_manager));
    assert_eq!(lease.identity().stored_account_id, Some(control.id));
    assert_eq!(lease.prompt_cache_discriminator(), None);
}

#[tokio::test]
async fn legacy_resume_preserves_the_original_control_cache_namespace_when_possible() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    let thread_id = ThreadId::new();
    let path = lease_path(codex_home.path(), thread_id);
    std::fs::create_dir_all(path.parent().expect("lease parent")).expect("create lease directory");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": LEGACY_LEASE_VERSION,
            "account_id": execution.id,
        }))
        .expect("serialize legacy lease"),
    )
    .expect("write legacy lease");

    let lease = ExecutionAccountLease::resolve(
        thread_id,
        control_manager,
        options(
            codex_home.path(),
            ExecutionAccountPooling::Enabled,
            ExecutionAccountStart::Resumed,
        ),
    )
    .await;

    assert_eq!(
        lease.identity().stored_account_id,
        Some(execution.id.clone())
    );
    assert_eq!(
        lease.prompt_cache_discriminator(),
        Some(execution.id.clone())
    );
    assert_eq!(
        read_persisted_lease_record(codex_home.path(), thread_id),
        Some(PersistedExecutionAccountLease {
            version: LEASE_VERSION,
            account_id: Some(execution.id),
            base_cache_identity: Some(format!("stored:{}", control.id)),
        })
    );
}

#[tokio::test]
async fn persisted_api_key_cache_identity_never_contains_the_raw_key() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let api_key = "sk-secret-execution-key";
    let control_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key(api_key));
    let thread_id = ThreadId::new();

    let lease = ExecutionAccountLease::resolve(
        thread_id,
        control_manager,
        options(
            codex_home.path(),
            ExecutionAccountPooling::Disabled,
            ExecutionAccountStart::New,
        ),
    )
    .await;

    assert_eq!(lease.prompt_cache_discriminator(), None);
    let contents = std::fs::read_to_string(lease_path(codex_home.path(), thread_id))
        .expect("read persisted lease");
    assert!(!contents.contains(api_key));
    assert!(contents.contains("credential:"));
}

#[tokio::test]
async fn codex_apps_auth_provider_fails_closed_after_api_key_identity_transition() {
    let (codex_home, control_manager, control, _execution) = test_accounts().await;
    let lease = ExecutionAccountLease::resolve(
        ThreadId::new(),
        control_manager,
        options(
            codex_home.path(),
            ExecutionAccountPooling::Disabled,
            ExecutionAccountStart::New,
        ),
    )
    .await;
    assert_eq!(lease.identity().stored_account_id, Some(control.id));

    let auth_provider = lease.codex_apps_auth_provider(lease.cache_identity());
    assert_eq!(
        auth_provider
            .to_auth_headers()
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer access-control")
    );

    lease.replace_auth_manager_for_testing(AuthManager::from_auth_for_testing(
        CodexAuth::from_api_key("sk-must-not-reach-apps"),
    ));

    assert!(auth_provider.to_auth_headers().is_empty());
}
