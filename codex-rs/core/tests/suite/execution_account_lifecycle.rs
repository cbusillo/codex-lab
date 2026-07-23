use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use anyhow::Result;
use base64::Engine as _;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::CodexThread;
use codex_core::ForkSnapshot;
use codex_core::NewThread;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_login::StoredAccount;
use codex_login::TokenData;
use codex_login::auth::save_auth;
use codex_login::token_data::IdTokenInfo;
use codex_models_manager::bundled_models_response;
use codex_protocol::ThreadId;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelServiceTier;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use pretty_assertions::assert_ne;
use serde_json::json;
use tempfile::TempDir;
use walkdir::WalkDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn chatgpt_tokens(account_id: &str) -> TokenData {
    chatgpt_tokens_with_access(account_id, &format!("access-{account_id}"))
}

fn chatgpt_tokens_with_access(account_id: &str, access_token: &str) -> TokenData {
    let header = json!({ "alg": "none", "typ": "JWT" });
    let payload = json!({
        "email": format!("{account_id}@example.com"),
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": format!("user-{account_id}"),
            "user_id": format!("user-{account_id}"),
        }
    });
    let encode = |value: &serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(value).expect("serialize test JWT segment"))
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
        access_token: access_token.to_string(),
        refresh_token: format!("refresh-{account_id}"),
        account_id: Some(account_id.to_string()),
    }
}

fn add_chatgpt_account(
    home: &Path,
    account_id: &str,
    label: &str,
    make_active: bool,
) -> Result<StoredAccount> {
    Ok(codex_login::upsert_chatgpt_account(
        home,
        AuthCredentialsStoreMode::File,
        chatgpt_tokens(account_id),
        Utc::now(),
        Some(label.to_string()),
        make_active,
    )?)
}

fn add_api_key_account(home: &Path, api_key: &str) -> Result<StoredAccount> {
    Ok(codex_login::upsert_api_key_account(
        home,
        AuthCredentialsStoreMode::File,
        api_key.to_string(),
        Some("API fallback".to_string()),
        /*make_active*/ false,
    )?)
}

fn rate_limit_window(resets_at: DateTime<Utc>) -> RateLimitWindow {
    RateLimitWindow {
        used_percent: 60.0,
        window_minutes: Some(300),
        resets_at: Some(resets_at.timestamp()),
    }
}

fn record_rate_limits(
    home: &Path,
    account: &StoredAccount,
    primary_resets_at: DateTime<Utc>,
    secondary_resets_at: Option<DateTime<Utc>>,
    recorded_at: DateTime<Utc>,
) -> Result<()> {
    codex_core::account_usage::record_rate_limit_snapshot(
        home,
        &account.id,
        RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: Some("Codex".to_string()),
            primary: Some(rate_limit_window(primary_resets_at)),
            secondary: secondary_resets_at.map(rate_limit_window),
            credits: None,
            individual_limit: None,
            plan_type: None,
            rate_limit_reached_type: None,
        },
        recorded_at,
    )?;
    Ok(())
}

fn execution_account_builder(home: Arc<TempDir>) -> TestCodexBuilder {
    test_codex()
        .with_home(home)
        .with_home_backed_auth_manager()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(|config| {
            config.auto_switch_accounts_on_rate_limit = true;
        })
}

fn model_catalog_builder(home: Arc<TempDir>) -> TestCodexBuilder {
    execution_account_builder(home).with_config(|config| {
        config.model = None;
        config.service_tier = Some("priority".to_string());
        config
            .features
            .enable(Feature::FastMode)
            .expect("test config should enable FastMode");
    })
}

fn lease_account_id(home: &Path, thread_id: ThreadId) -> Result<String> {
    let lease_path = home
        .join("execution-account-leases")
        .join(format!("{thread_id}.json"));
    let lease: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(lease_path)?)?;
    Ok(lease["account_id"]
        .as_str()
        .expect("execution account lease should contain an account ID")
        .to_string())
}

async fn submit_text(thread: &CodexThread, text: &str) -> Result<()> {
    thread
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    Ok(())
}

fn account_model(slug: &str, service_tiers: &[&str]) -> ModelInfo {
    let mut model = bundled_models_response()
        .expect("bundled models should load")
        .models
        .into_iter()
        .next()
        .expect("bundled models should not be empty");
    model.slug = slug.to_string();
    model.display_name = slug.to_string();
    model.base_instructions = format!("instructions for {slug}");
    model.visibility = ModelVisibility::List;
    model.priority = 0;
    model.service_tiers = service_tiers
        .iter()
        .map(|service_tier| ModelServiceTier {
            id: (*service_tier).to_string(),
            name: (*service_tier).to_string(),
            description: format!("{service_tier} service tier"),
        })
        .collect();
    model.default_service_tier = None;
    model
}

async fn mount_models_for_authorization(
    server: &MockServer,
    authorization: String,
    models: Vec<ModelInfo>,
) {
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", authorization))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(ModelsResponse { models }),
        )
        .expect(1)
        .mount(server)
        .await;
}

fn cached_model_catalogs(home: &Path) -> Result<Vec<Vec<String>>> {
    let mut catalogs = Vec::new();
    for entry in WalkDir::new(home.join("models-cache")) {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.file_name() != "models_cache.json" {
            continue;
        }
        let catalog: ModelsResponse =
            serde_json::from_str(&std::fs::read_to_string(entry.path())?)?;
        catalogs.push(catalog.models.into_iter().map(|model| model.slug).collect());
    }
    Ok(catalogs)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_and_failover_prefer_the_soonest_active_chatgpt_window() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let control = add_chatgpt_account(
        home.path(),
        "account_id",
        "Control",
        /*make_active*/ true,
    )?;
    let primary_candidate = add_chatgpt_account(
        home.path(),
        "primary-candidate",
        "Primary candidate",
        /*make_active*/ false,
    )?;
    let secondary_candidate = add_chatgpt_account(
        home.path(),
        "secondary-candidate",
        "Secondary candidate",
        /*make_active*/ false,
    )?;
    let api_key = "sk-api-fallback";
    let _api_key_account = add_api_key_account(home.path(), api_key)?;
    let now = Utc::now();

    record_rate_limits(
        home.path(),
        &control,
        now + Duration::hours(4),
        Some(now + Duration::minutes(10)),
        now,
    )?;
    record_rate_limits(
        home.path(),
        &primary_candidate,
        now + Duration::minutes(25),
        Some(now + Duration::hours(4)),
        now,
    )?;
    record_rate_limits(
        home.path(),
        &secondary_candidate,
        now + Duration::hours(3),
        Some(now + Duration::minutes(20)),
        now,
    )?;

    let rate_limit_response = ResponseTemplate::new(429).set_body_json(json!({
        "error": {
            "type": "usage_limit_reached",
            "message": "limit reached",
            "resets_at": (now + Duration::hours(2)).timestamp(),
            "plan_type": "pro",
        }
    }));
    let recovered_response = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse(vec![
            ev_assistant_message("msg-2", "recovered"),
            ev_completed("resp-2"),
        ]));
    let response_mock =
        mount_response_sequence(&server, vec![rate_limit_response, recovered_response]).await;

    let mut builder = execution_account_builder(home.clone()).with_config(|config| {
        config.api_key_fallback_on_all_accounts_limited = true;
    });
    let fixture = builder.build(&server).await?;
    let thread_id = fixture.session_configured.thread_id;
    assert_eq!(lease_account_id(home.path(), thread_id)?, control.id);

    submit_text(&fixture.codex, "trigger a usage limit").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer Access Token".to_string())
    );
    assert_eq!(
        requests[1].header("authorization"),
        Some("Bearer access-secondary-candidate".to_string())
    );
    assert_eq!(
        requests[0].body_json()["prompt_cache_key"],
        thread_id.to_string()
    );
    assert_eq!(
        requests[1].body_json()["prompt_cache_key"],
        format!("{thread_id}:{}", secondary_candidate.id)
    );
    assert_ne!(
        requests[0].body_json()["prompt_cache_key"],
        requests[1].body_json()["prompt_cache_key"]
    );
    assert!(
        requests.iter().all(|request| {
            request.header("authorization") != Some(format!("Bearer {api_key}"))
        })
    );
    assert_eq!(
        lease_account_id(home.path(), thread_id)?,
        secondary_candidate.id
    );
    assert_eq!(
        codex_login::get_active_account_id(home.path(), AuthCredentialsStoreMode::File)?,
        Some(control.id)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_cache_key_survives_control_switch_detach_and_reattach() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let control = add_chatgpt_account(
        home.path(),
        "account_id",
        "Control",
        /*make_active*/ true,
    )?;
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
            sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
            sse(vec![ev_response_created("resp-3"), ev_completed("resp-3")]),
        ],
    )
    .await;
    let mut builder = execution_account_builder(home.clone());
    let fixture = builder.build(&server).await?;
    let thread_id = fixture.session_configured.thread_id;
    submit_text(&fixture.codex, "before control switch").await?;

    let alternate = add_chatgpt_account(
        home.path(),
        "alternate-control",
        "Alternate control",
        /*make_active*/ false,
    )?;
    let (_account, alternate_auth) =
        codex_login::auth_for_account(home.path(), AuthCredentialsStoreMode::File, &alternate.id)?;
    save_auth(home.path(), &alternate_auth, AuthCredentialsStoreMode::File)?;
    codex_login::set_active_account_id(
        home.path(),
        AuthCredentialsStoreMode::File,
        Some(alternate.id),
    )?;
    fixture
        .thread_manager
        .reload_auth_for_loaded_threads()
        .await;
    submit_text(&fixture.codex, "while detached").await?;

    let (_account, control_auth) =
        codex_login::auth_for_account(home.path(), AuthCredentialsStoreMode::File, &control.id)?;
    save_auth(home.path(), &control_auth, AuthCredentialsStoreMode::File)?;
    codex_login::set_active_account_id(
        home.path(),
        AuthCredentialsStoreMode::File,
        Some(control.id),
    )?;
    fixture
        .thread_manager
        .reload_auth_for_loaded_threads()
        .await;
    submit_text(&fixture.codex, "after reattach").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.body_json()["prompt_cache_key"].clone())
            .collect::<Vec<_>>(),
        vec![json!(thread_id.to_string()); 3]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_and_forked_threads_reuse_the_source_execution_lease() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let control = add_chatgpt_account(
        home.path(),
        "account_id",
        "Control",
        /*make_active*/ true,
    )?;
    let execution = add_chatgpt_account(
        home.path(),
        "execution",
        "Execution",
        /*make_active*/ false,
    )?;
    let initial_time = Utc::now();
    record_rate_limits(
        home.path(),
        &control,
        initial_time + Duration::hours(4),
        None,
        initial_time,
    )?;
    record_rate_limits(
        home.path(),
        &execution,
        initial_time + Duration::minutes(10),
        None,
        initial_time,
    )?;

    let response_mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
            sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
            sse(vec![ev_response_created("resp-3"), ev_completed("resp-3")]),
        ],
    )
    .await;

    let mut source_builder = execution_account_builder(home.clone());
    let source = source_builder.build(&server).await?;
    let source_thread_id = source.session_configured.thread_id;
    submit_text(&source.codex, "source execution account").await?;
    let source_rollout_path = source
        .codex
        .rollout_path()
        .expect("source thread should have a rollout path");
    assert_eq!(
        lease_account_id(home.path(), source_thread_id)?,
        execution.id
    );
    source.codex.shutdown_and_wait().await?;

    let changed_time = Utc::now();
    record_rate_limits(
        home.path(),
        &control,
        changed_time + Duration::minutes(5),
        None,
        changed_time,
    )?;
    record_rate_limits(
        home.path(),
        &execution,
        changed_time + Duration::hours(3),
        None,
        changed_time,
    )?;

    let mut resumed_builder = execution_account_builder(home.clone());
    let resumed = resumed_builder
        .resume(&server, home.clone(), source_rollout_path.clone())
        .await?;
    assert_eq!(resumed.session_configured.thread_id, source_thread_id);
    submit_text(&resumed.codex, "resumed execution account").await?;
    assert_eq!(
        lease_account_id(home.path(), source_thread_id)?,
        execution.id
    );

    let NewThread {
        thread_id: forked_thread_id,
        thread: forked,
        ..
    } = resumed
        .thread_manager
        .fork_thread(
            ForkSnapshot::Interrupted,
            resumed.config.clone(),
            source_rollout_path,
            /*thread_source*/ None,
            /*parent_trace*/ None,
        )
        .await
        .expect("fork should preserve the source execution lease");
    assert_ne!(forked_thread_id, source_thread_id);
    submit_text(&forked, "forked execution account").await?;
    assert_eq!(
        lease_account_id(home.path(), forked_thread_id)?,
        execution.id
    );

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(
            request.header("authorization"),
            Some("Bearer access-execution".to_string())
        );
    }
    assert_eq!(
        requests[0].body_json()["prompt_cache_key"],
        format!("{source_thread_id}:{}", execution.id)
    );
    assert_eq!(
        requests[1].body_json()["prompt_cache_key"],
        requests[0].body_json()["prompt_cache_key"]
    );
    assert_eq!(
        requests[2].body_json()["prompt_cache_key"],
        format!("{forked_thread_id}:{}", execution.id)
    );

    resumed.codex.shutdown_and_wait().await?;
    forked.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_catalog_cache_isolated_per_execution_account_across_restart() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    let control = add_chatgpt_account(
        home.path(),
        "account_id",
        "Control",
        /*make_active*/ true,
    )?;
    let account_a = add_chatgpt_account(
        home.path(),
        "catalog-a",
        "Catalog A",
        /*make_active*/ false,
    )?;
    let account_b = add_chatgpt_account(
        home.path(),
        "catalog-b",
        "Catalog B",
        /*make_active*/ false,
    )?;
    let initial_time = Utc::now();
    record_rate_limits(
        home.path(),
        &control,
        initial_time + Duration::hours(5),
        None,
        initial_time,
    )?;
    record_rate_limits(
        home.path(),
        &account_a,
        initial_time + Duration::minutes(10),
        None,
        initial_time,
    )?;
    record_rate_limits(
        home.path(),
        &account_b,
        initial_time + Duration::minutes(30),
        None,
        initial_time,
    )?;

    mount_models_for_authorization(
        &server,
        "Bearer access-catalog-a".to_string(),
        vec![account_model("catalog-a-model", &["priority"])],
    )
    .await;
    let mut first_builder = model_catalog_builder(home.clone());
    let first = first_builder.build(&server).await?;
    assert_eq!(first.session_configured.model, "catalog-a-model");
    assert_eq!(
        first.session_configured.service_tier.as_deref(),
        Some("priority")
    );
    first.codex.shutdown_and_wait().await?;

    let changed_time = Utc::now();
    record_rate_limits(
        home.path(),
        &control,
        changed_time + Duration::hours(5),
        None,
        changed_time,
    )?;
    record_rate_limits(
        home.path(),
        &account_a,
        changed_time + Duration::hours(3),
        None,
        changed_time,
    )?;
    record_rate_limits(
        home.path(),
        &account_b,
        changed_time + Duration::minutes(5),
        None,
        changed_time,
    )?;

    mount_models_for_authorization(
        &server,
        "Bearer access-catalog-b".to_string(),
        vec![account_model("catalog-b-model", &[])],
    )
    .await;
    let mut second_builder = model_catalog_builder(home.clone());
    let second = second_builder.build(&server).await?;
    assert_eq!(second.session_configured.model, "catalog-b-model");
    assert_eq!(second.session_configured.service_tier, None);

    let model_authorizations = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| request.method.as_str() == "GET" && request.url.path() == "/v1/models")
        .map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .expect("model catalog request should include authorization")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        model_authorizations,
        vec![
            "Bearer access-catalog-a".to_string(),
            "Bearer access-catalog-b".to_string(),
        ]
    );

    let cached_catalogs = cached_model_catalogs(home.path())?;
    assert_eq!(cached_catalogs.len(), 2);
    assert!(cached_catalogs.contains(&vec!["catalog-a-model".to_string()]));
    assert!(cached_catalogs.contains(&vec!["catalog-b-model".to_string()]));

    second.codex.shutdown_and_wait().await?;
    Ok(())
}

#[derive(Clone)]
struct StaleExecutionCatalogResponder {
    auth_home: PathBuf,
    refreshed_access_token: String,
    stale_authorization: String,
    refreshed_authorization: String,
    models_etag: String,
    calls: Arc<AtomicUsize>,
}

impl Respond for StaleExecutionCatalogResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let authorization = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok());
        match call {
            0 => {
                assert_eq!(authorization, Some(self.stale_authorization.as_str()));
                // The execution account's token is refreshed out-of-band before
                // we reject the stale request, mirroring a real token rotation.
                codex_login::upsert_chatgpt_account(
                    &self.auth_home,
                    AuthCredentialsStoreMode::File,
                    chatgpt_tokens_with_access("execution", &self.refreshed_access_token),
                    Utc::now(),
                    Some("Execution".to_string()),
                    /*make_active*/ false,
                )
                .expect("refresh execution catalog account");
                ResponseTemplate::new(401).set_body_string("stale execution token")
            }
            1 => {
                assert_eq!(authorization, Some(self.refreshed_authorization.as_str()));
                // A mismatched models etag forces the catalog to be refreshed,
                // which must now use the refreshed execution auth.
                sse_response(sse(vec![
                    ev_response_created("resp-2"),
                    ev_assistant_message("msg-1", "done"),
                    ev_completed("resp-2"),
                ]))
                .insert_header("X-Models-Etag", self.models_etag.as_str())
            }
            other => panic!("unexpected responses request {other}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_catalog_refresh_after_401_uses_refreshed_execution_auth() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    let control = add_chatgpt_account(
        home.path(),
        "account_id",
        "Control",
        /*make_active*/ true,
    )?;
    let execution = add_chatgpt_account(
        home.path(),
        "execution",
        "Execution",
        /*make_active*/ false,
    )?;
    let now = Utc::now();
    // The control account is limited, so the execution account backs the session.
    codex_core::account_usage::record_usage_limit_hint(
        home.path(),
        &control.id,
        /*plan*/ None,
        Some(now + Duration::hours(1)),
        now,
        /*reached_type*/ None,
    )?;

    let stale_authorization = "Bearer access-execution".to_string();
    let refreshed_access_token = "access-execution-refreshed".to_string();
    let refreshed_authorization = format!("Bearer {refreshed_access_token}");

    // Spawn catalog fetch is served to the stale execution token; the etag-driven
    // refresh must be served only to the refreshed execution token.
    mount_models_for_authorization(
        &server,
        stale_authorization.clone(),
        vec![account_model("catalog-exec", &["priority"])],
    )
    .await;
    mount_models_for_authorization(
        &server,
        refreshed_authorization.clone(),
        vec![account_model("catalog-exec-refreshed", &["priority"])],
    )
    .await;

    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(StaleExecutionCatalogResponder {
            auth_home: home.path().to_path_buf(),
            refreshed_access_token: refreshed_access_token.clone(),
            stale_authorization: stale_authorization.clone(),
            refreshed_authorization: refreshed_authorization.clone(),
            models_etag: "\"models-etag-refreshed\"".to_string(),
            calls: Arc::clone(&calls),
        })
        .expect(2)
        .mount(&server)
        .await;

    let mut builder = model_catalog_builder(home.clone());
    let fixture = builder.build(&server).await?;
    assert_eq!(fixture.session_configured.model, "catalog-exec");

    submit_text(&fixture.codex, "trigger a catalog refresh").await?;

    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let model_authorizations = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| request.method.as_str() == "GET" && request.url.path() == "/v1/models")
        .map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .expect("model catalog request should include authorization")
                .to_string()
        })
        .collect::<Vec<_>>();
    // The catalog was fetched with the stale token on spawn and refreshed with
    // the rotated execution token; the control token was never used.
    assert!(
        model_authorizations
            .iter()
            .any(|authorization| authorization == &refreshed_authorization),
        "catalog refresh never used the refreshed execution auth: {model_authorizations:?}"
    );
    assert!(
        model_authorizations
            .iter()
            .all(|authorization| authorization != "Bearer Access Token"),
        "catalog fetch leaked the control account's auth: {model_authorizations:?}"
    );

    // The control auth manager is untouched by the execution-account refresh.
    assert_eq!(
        fixture
            .thread_manager
            .auth_manager()
            .auth_cached()
            .expect("control auth should remain loaded")
            .get_token()?,
        "Access Token"
    );
    assert_eq!(
        codex_login::find_account(home.path(), AuthCredentialsStoreMode::File, &execution.id,)?
            .and_then(|account| account.tokens)
            .map(|tokens| tokens.access_token),
        Some(refreshed_access_token)
    );
    assert_eq!(
        codex_login::get_active_account_id(home.path(), AuthCredentialsStoreMode::File)?,
        Some(control.id)
    );

    fixture.codex.shutdown_and_wait().await?;
    Ok(())
}
