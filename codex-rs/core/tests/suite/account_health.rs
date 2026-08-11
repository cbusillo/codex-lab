use std::ffi::OsString;
use std::sync::Arc;

use anyhow::Result;
use base64::Engine;
use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AccountHealth;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;
use codex_login::StoredAccount;
use codex_login::TokenData;
use codex_login::token_data::IdTokenInfo;
use codex_protocol::auth::RefreshTokenFailedReason;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: String) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: this test is serialized with every other auth environment test.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: restore the environment before another auth environment test can run.
        unsafe {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn tokens(account_id: &str, refresh_token: &str) -> TokenData {
    let encode = |value: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(value).expect("serialize test JWT segment"))
    };
    let header = json!({"alg": "none", "typ": "JWT"});
    let payload = json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": account_id }
    });
    TokenData {
        id_token: IdTokenInfo {
            chatgpt_account_id: Some(account_id.to_string()),
            raw_jwt: format!("{}.{}.signature", encode(&header), encode(&payload)),
            ..Default::default()
        },
        access_token: format!("access-{account_id}"),
        refresh_token: refresh_token.to_string(),
        account_id: Some(account_id.to_string()),
    }
}

fn add_account(home: &TempDir, account_id: &str) -> Result<StoredAccount> {
    Ok(codex_login::upsert_chatgpt_account(
        home.path(),
        AuthCredentialsStoreMode::File,
        tokens(account_id, &format!("refresh-{account_id}")),
        Utc::now(),
        Some(account_id.to_string()),
        /*make_active*/ false,
    )?)
}

async fn catalog_manager(home: &TempDir, account: &StoredAccount) -> Result<Arc<AuthManager>> {
    Ok(AuthManager::for_catalog_account(
        home.path().to_path_buf(),
        account.id.clone(),
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        /*forced_chatgpt_workspace_id*/ None,
        codex_login::test_support::transport_default_auth_route_config(),
    )
    .await?)
}

#[serial_test::serial(auth_env)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_refresh_failure_isolates_only_that_account_from_startup_selection() -> Result<()>
{
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let _endpoint_guard = EnvGuard::set(
        REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{}/oauth/token", server.uri()),
    );
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "code": "refresh_token_reused" }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let home = Arc::new(TempDir::new()?);
    let first = add_account(&home, "account-a")?;
    let second = add_account(&home, "account-b")?;
    // Preferred selection ties break by catalog ID, so the unhealthy account would otherwise win.
    let (invalid, healthy) = if first.id < second.id {
        (first, second)
    } else {
        (second, first)
    };
    codex_login::activate_account(
        home.path(),
        &healthy.id,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let healthy_tokens = healthy.tokens.as_ref().expect("healthy account tokens");
    let healthy_account_id = healthy_tokens.account_id.as_deref().expect("account id");
    let healthy_access_token = healthy_tokens.id_token.raw_jwt.clone();
    let invalid_error = catalog_manager(&home, &invalid)
        .await?
        .refresh_token_from_authority()
        .await
        .expect_err("reused refresh token should fail");
    assert_eq!(
        invalid_error.failed_reason(),
        Some(RefreshTokenFailedReason::Exhausted)
    );
    let accounts = codex_login::list_accounts(home.path(), AuthCredentialsStoreMode::File)?;
    let health = |account_id: &str| {
        accounts
            .iter()
            .find(|account| account.id == account_id)
            .map(|account| account.health)
    };
    assert_eq!(health(&invalid.id), Some(AccountHealth::ReauthRequired));
    assert_eq!(health(&healthy.id), Some(AccountHealth::Ok));
    assert_eq!(
        codex_login::get_active_account_id(home.path(), AuthCredentialsStoreMode::File)?,
        Some(healthy.id.clone())
    );

    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;
    let mut builder = test_codex()
        .with_home(home)
        .with_home_backed_auth_manager()
        .with_auth(CodexAuth::from_external_chatgpt_tokens(
            &healthy_access_token,
            healthy_account_id,
            /*chatgpt_plan_type*/ None,
        )?)
        .with_config(|config| config.auto_switch_accounts_on_rate_limit = true);
    let fixture = builder.build_with_auto_env(&server).await?;
    fixture
        .codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        response_mock.single_request().header("authorization"),
        Some(format!("Bearer {healthy_access_token}"))
    );
    server.verify().await;
    Ok(())
}
