use super::*;

use base64::Engine;
use chrono::Duration;
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
    )
    .expect("save control auth");
    let control_manager = Arc::new(
        AuthManager::new(
            codex_home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*chatgpt_base_url*/ None,
        )
        .await,
    );
    (codex_home, control_manager, control, execution)
}

fn options(codex_home: &Path) -> ExecutionAccountOptions {
    ExecutionAccountOptions {
        codex_home: codex_home.to_path_buf(),
        auth_home: codex_home.to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        chatgpt_base_url: "https://chatgpt.com/backend-api/".to_string(),
        allow_api_key_fallback: false,
        pooled: true,
    }
}

#[tokio::test]
async fn lease_prefers_reset_soonest_and_stays_pinned_without_changing_control() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    let now = Utc::now();
    crate::account_usage::record_rate_limit_snapshot(
        codex_home.path(),
        &control.id,
        rate_limit_snapshot(now + Duration::hours(4), 40.0),
        now,
    )
    .expect("record control usage");
    crate::account_usage::record_rate_limit_snapshot(
        codex_home.path(),
        &execution.id,
        rate_limit_snapshot(now + Duration::hours(1), 40.0),
        now,
    )
    .expect("record execution usage");
    let thread_id = ThreadId::default();

    let first = ExecutionAccountLease::resolve(
        thread_id,
        Arc::clone(&control_manager),
        options(codex_home.path()),
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
        options(codex_home.path()),
    )
    .await;
    assert_eq!(
        resumed.identity().stored_account_id,
        Some(execution.id.clone())
    );
}

#[tokio::test]
async fn usage_limit_failover_changes_only_execution_lease() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    let now = Utc::now();
    crate::account_usage::record_rate_limit_snapshot(
        codex_home.path(),
        &control.id,
        rate_limit_snapshot(now + Duration::hours(4), 40.0),
        now,
    )
    .expect("record control usage");
    crate::account_usage::record_rate_limit_snapshot(
        codex_home.path(),
        &execution.id,
        rate_limit_snapshot(now + Duration::hours(1), 40.0),
        now,
    )
    .expect("record execution usage");
    let lease = ExecutionAccountLease::resolve(
        ThreadId::default(),
        Arc::clone(&control_manager),
        options(codex_home.path()),
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
    assert_ne!(lease.auth_revision(), previous_revision);
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
async fn pooled_execution_lease_is_isolated_from_control_account_switch() {
    let (codex_home, control_manager, control, execution) = test_accounts().await;
    let thread_id = ThreadId::default();
    persist_lease(codex_home.path(), thread_id, &control.id).expect("persist control lease");
    let lease = ExecutionAccountLease::resolve(
        thread_id,
        Arc::clone(&control_manager),
        options(codex_home.path()),
    )
    .await;
    assert_eq!(lease.identity().stored_account_id, Some(control.id.clone()));
    assert!(!Arc::ptr_eq(&lease.auth_manager(), &control_manager));

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
    )
    .expect("save switched control auth");
    codex_login::set_active_account_id(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        Some(execution.id.clone()),
    )
    .expect("switch control account");

    lease.prepare_for_control_auth_reload().await;
    control_manager.reload().await;
    lease.reconcile_after_control_auth_reload().await;

    assert_eq!(
        control_manager
            .auth_cached()
            .and_then(|auth| auth.get_account_id()),
        Some("execution".to_string())
    );
    assert_eq!(lease.identity().stored_account_id, Some(control.id));
    assert_eq!(
        lease
            .auth_manager()
            .auth_cached()
            .and_then(|auth| auth.get_account_id()),
        Some("control".to_string())
    );
}
