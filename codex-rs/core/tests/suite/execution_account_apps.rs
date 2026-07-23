use base64::Engine as _;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::ForkSnapshot;
use codex_core::NewThread;
use codex_core::config::Config;
use codex_features::Feature;
use codex_login::AuthDotJson;
use codex_login::CodexAuth;
use codex_login::StoredAccount;
use codex_login::TokenData;
use codex_login::token_data::IdTokenInfo;
use codex_models_manager::bundled_models_response;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::PathExt;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_compact_user_history_with_summary_once;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::Respond;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::path_regex;

const CONTROL_STORED_ACCOUNT_ID: &str = "stored-0-control";
const CONTROL_CHATGPT_ACCOUNT_ID: &str = "account_id";
const CONTROL_AUTHORIZATION: &str = "Bearer Access Token";
const CONTROL_CONNECTOR_NAME: &str = "Control Calendar";
const CONTROL_NAMESPACE: &str = "mcp__codex_apps__control_calendar";
const CONTROL_TOOL_MARKER: &str = "CONTROL_ACCOUNT_ONLY_TOOL";
const EXECUTION_STORED_ACCOUNT_ID: &str = "stored-1-execution";
const EXECUTION_CHATGPT_ACCOUNT_ID: &str = "execution-account";
const EXECUTION_CONNECTOR_NAME: &str = "Execution Mail";
const EXECUTION_NAMESPACE: &str = "mcp__codex_apps__execution_mail";
const EXECUTION_TOOL_MARKER: &str = "EXECUTION_ACCOUNT_ONLY_TOOL";
const LOOKUP_TOOL_NAME: &str = "_lookup";
const MODEL: &str = "gpt-5.4";

#[derive(Clone)]
struct AccountAppsResponder {
    execution_authorizations: Vec<String>,
    control_tools_enabled: bool,
    execution_tools_enabled: bool,
}

impl Respond for AccountAppsResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Some(authorization) = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
        else {
            return ResponseTemplate::new(401);
        };
        let profile = if authorization == CONTROL_AUTHORIZATION {
            AppProfile {
                connector_id: "control_calendar",
                connector_name: CONTROL_CONNECTOR_NAME,
                tool_name: "control_calendar_lookup",
                tool_marker: CONTROL_TOOL_MARKER,
                tools_enabled: self.control_tools_enabled,
            }
        } else if self
            .execution_authorizations
            .iter()
            .any(|candidate| authorization == candidate)
        {
            AppProfile {
                connector_id: "execution_mail",
                connector_name: EXECUTION_CONNECTOR_NAME,
                tool_name: "execution_mail_lookup",
                tool_marker: EXECUTION_TOOL_MARKER,
                tools_enabled: self.execution_tools_enabled,
            }
        } else {
            return ResponseTemplate::new(401);
        };

        let body: Value = match serde_json::from_slice(&request.body) {
            Ok(body) => body,
            Err(error) => {
                return ResponseTemplate::new(400).set_body_json(json!({
                    "error": format!("invalid JSON-RPC body: {error}"),
                }));
            }
        };
        let Some(rpc_method) = body.get("method").and_then(Value::as_str) else {
            return ResponseTemplate::new(400);
        };
        let id = body.get("id").cloned().unwrap_or(Value::Null);

        match rpc_method {
            "initialize" => ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": body
                        .pointer("/params/protocolVersion")
                        .and_then(Value::as_str)
                        .unwrap_or("2025-11-25"),
                    "capabilities": { "tools": { "listChanged": true } },
                    "serverInfo": { "name": "account-apps", "version": "1.0.0" }
                }
            })),
            "notifications/initialized" => ResponseTemplate::new(202),
            "tools/list" => {
                let tools = profile.tools_enabled.then(|| {
                    json!({
                        "name": profile.tool_name,
                        "description": profile.tool_marker,
                        "annotations": { "readOnlyHint": true },
                        "inputSchema": {
                            "type": "object",
                            "properties": {},
                            "additionalProperties": false
                        },
                        "_meta": {
                            "connector_id": profile.connector_id,
                            "connector_name": profile.connector_name,
                            "connector_description": profile.tool_marker,
                            "_codex_apps": {
                                "resource_uri": format!(
                                    "connector://{}/tools/{}",
                                    profile.connector_id, profile.tool_name
                                ),
                                "contains_mcp_source": true,
                                "connector_id": profile.connector_id
                            }
                        }
                    })
                });
                ResponseTemplate::new(200).set_body_json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                    "tools": tools.into_iter().collect::<Vec<_>>(),
                    "nextCursor": null
                    }
                }))
            }
            "tools/call" => ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": profile.tool_marker }],
                    "isError": false
                }
            })),
            method if method.starts_with("notifications/") => ResponseTemplate::new(202),
            _ => ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "method not found" }
            })),
        }
    }
}

struct AppProfile {
    connector_id: &'static str,
    connector_name: &'static str,
    tool_name: &'static str,
    tool_marker: &'static str,
    tools_enabled: bool,
}

struct Accounts {
    execution_authorization: String,
}

fn fake_chatgpt_token_data(account_id: &str) -> TokenData {
    fake_chatgpt_token_data_with_access(account_id, &format!("access-{account_id}"))
}

fn fake_chatgpt_token_data_with_access(account_id: &str, access_token: &str) -> TokenData {
    let header = json!({ "alg": "none", "typ": "JWT" });
    let payload = json!({
        "email": "user@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": format!("user-{account_id}")
        }
    });
    let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let raw_jwt = format!(
        "{}.{}.{}",
        encode(&serde_json::to_vec(&header).expect("serialize JWT header")),
        encode(&serde_json::to_vec(&payload).expect("serialize JWT payload")),
        encode(b"sig")
    );
    TokenData {
        id_token: IdTokenInfo {
            email: Some("user@example.com".to_string()),
            chatgpt_plan_type: None,
            chatgpt_user_id: Some(format!("user-{account_id}")),
            chatgpt_account_id: Some(account_id.to_string()),
            chatgpt_account_is_fedramp: false,
            raw_jwt,
        },
        access_token: access_token.to_string(),
        refresh_token: format!("refresh-{account_id}"),
        account_id: Some(account_id.to_string()),
    }
}

fn write_accounts(home: &TempDir) -> anyhow::Result<Accounts> {
    let accounts = vec![
        StoredAccount {
            id: CONTROL_STORED_ACCOUNT_ID.to_string(),
            mode: codex_app_server_protocol::AuthMode::Chatgpt,
            label: Some("Control".to_string()),
            openai_api_key: None,
            tokens: Some(fake_chatgpt_token_data(CONTROL_CHATGPT_ACCOUNT_ID)),
            last_refresh: Some(chrono::Utc::now()),
            created_at: None,
            last_used_at: None,
        },
        StoredAccount {
            id: EXECUTION_STORED_ACCOUNT_ID.to_string(),
            mode: codex_app_server_protocol::AuthMode::Chatgpt,
            label: Some("Execution".to_string()),
            openai_api_key: None,
            tokens: Some(fake_chatgpt_token_data(EXECUTION_CHATGPT_ACCOUNT_ID)),
            last_refresh: Some(chrono::Utc::now()),
            created_at: None,
            last_used_at: None,
        },
    ];
    std::fs::write(
        home.path().join("auth_accounts.json"),
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "active_account_id": CONTROL_STORED_ACCOUNT_ID,
            "accounts": accounts,
        }))?,
    )?;
    Ok(Accounts {
        execution_authorization: format!("Bearer access-{EXECUTION_CHATGPT_ACCOUNT_ID}"),
    })
}

async fn mount_models(server: &MockServer, authorization: &str) {
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", authorization.to_string()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                bundled_models_response().expect("bundled model catalog should load"),
            ),
        )
        .up_to_n_times(4)
        .mount(server)
        .await;
}

async fn mount_account_apps(
    server: &MockServer,
    execution_authorizations: Vec<String>,
    control_tools_enabled: bool,
    execution_tools_enabled: bool,
) {
    Mock::given(method("POST"))
        .and(path_regex("^/api/codex/apps/?$"))
        .respond_with(AccountAppsResponder {
            execution_authorizations,
            control_tools_enabled,
            execution_tools_enabled,
        })
        .mount(server)
        .await;
}

fn assert_request_uses_only_account_apps(
    request: &ResponsesRequest,
    expected_tool_marker: &str,
    forbidden_tool_marker: &str,
) {
    let developer_text = request.message_input_texts("developer").join("\n");
    assert!(
        developer_text.contains("<apps_instructions>")
            || developer_text.contains("state: available"),
        "expected model-visible Apps availability context: {developer_text}"
    );

    let tools = request.body_json()["tools"].to_string();
    assert!(
        tools.contains(expected_tool_marker),
        "missing expected Apps tool marker {expected_tool_marker}: {tools}"
    );
    assert!(
        !tools.contains(forbidden_tool_marker),
        "retained forbidden Apps tool marker {forbidden_tool_marker}: {tools}"
    );
}

fn assert_request_has_no_apps(request: &ResponsesRequest) {
    let developer_text = request.message_input_texts("developer").join("\n");
    assert_eq!(developer_text.matches("<apps_instructions>").count(), 0);
    assert_request_has_no_apps_tools(request);
}

fn assert_request_has_no_apps_tools(request: &ResponsesRequest) {
    let tools = request.body_json()["tools"].to_string();
    assert!(!tools.contains(CONTROL_TOOL_MARKER));
    assert!(!tools.contains(EXECUTION_TOOL_MARKER));
}

fn approve_account_app_tools(config: &mut Config) {
    let user_config_path = config.codex_home.join("config.toml").abs();
    let user_config = toml::from_str(
        r#"
[apps.control_calendar]
default_tools_approval_mode = "approve"

[apps.execution_mail]
default_tools_approval_mode = "approve"
"#,
    )
    .expect("apps config should parse");
    config.config_layer_stack = config
        .config_layer_stack
        .with_user_config(&user_config_path, user_config);
}

fn apps_authorizations(requests: &[Request]) -> Vec<String> {
    requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST" && request.url.path() == "/api/codex/apps"
        })
        .filter_map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect()
}

fn request_body_json(request: &Request) -> Option<Value> {
    let body = if request
        .headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().eq_ignore_ascii_case("zstd"))
        }) {
        zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()?
    } else {
        request.body.clone()
    };
    serde_json::from_slice(&body).ok()
}

fn apps_tool_call_authorizations(requests: &[Request]) -> Vec<String> {
    requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST"
                && request.url.path() == "/api/codex/apps"
                && request_body_json(request)
                    .and_then(|body| {
                        body.get("method")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .as_deref()
                    == Some("tools/call")
        })
        .filter_map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect()
}

#[derive(Clone)]
struct RefreshExecutionAccountResponder {
    auth_home: PathBuf,
    access_token: String,
    stale_authorization: String,
    refreshed_authorization: String,
    calls: Arc<AtomicUsize>,
}

impl Respond for RefreshExecutionAccountResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let authorization = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok());
        match call {
            0 => {
                assert_eq!(authorization, Some(self.stale_authorization.as_str()));
                codex_login::upsert_chatgpt_account(
                    &self.auth_home,
                    AuthCredentialsStoreMode::File,
                    fake_chatgpt_token_data_with_access(
                        EXECUTION_CHATGPT_ACCOUNT_ID,
                        &self.access_token,
                    ),
                    chrono::Utc::now(),
                    Some("Execution".to_string()),
                    /*make_active*/ false,
                )
                .expect("refresh execution catalog account");
                ResponseTemplate::new(401).set_body_string("stale execution token")
            }
            1 => {
                assert_eq!(authorization, Some(self.refreshed_authorization.as_str()));
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(vec![
                        ev_response_created("resp-refresh-2"),
                        ev_function_call_with_namespace(
                            "refreshed-execution-app-call",
                            EXECUTION_NAMESPACE,
                            LOOKUP_TOOL_NAME,
                            "{}",
                        ),
                        ev_completed("resp-refresh-2"),
                    ]))
            }
            2 => {
                assert_eq!(authorization, Some(self.refreshed_authorization.as_str()));
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse(vec![
                        ev_assistant_message("msg-refresh", "done"),
                        ev_completed("resp-refresh-3"),
                    ]))
            }
            _ => panic!("unexpected refreshed execution response request {call}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn divergent_control_and_execution_accounts_use_execution_apps_context() -> anyhow::Result<()>
{
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    let accounts = write_accounts(home.as_ref())?;
    let now = chrono::Utc::now();
    codex_core::account_usage::record_usage_limit_hint(
        home.path(),
        CONTROL_STORED_ACCOUNT_ID,
        /*plan*/ None,
        Some(now + chrono::Duration::hours(1)),
        now,
        /*reached_type*/ None,
    )?;

    mount_models(&server, &accounts.execution_authorization).await;
    mount_account_apps(
        &server,
        vec![accounts.execution_authorization.clone()],
        /*control_tools_enabled*/ false,
        /*execution_tools_enabled*/ true,
    )
    .await;
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
            sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
        ],
    )
    .await;
    let apps_base_url = server.uri();
    let fixture = test_codex()
        .with_home(home.clone())
        .with_home_backed_auth_manager()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.auto_switch_accounts_on_rate_limit = true;
            config.model = Some(MODEL.to_string());
            config.chatgpt_base_url = apps_base_url;
            config
                .features
                .enable(Feature::Apps)
                .expect("Apps feature should be configurable");
            config
                .features
                .disable(Feature::ToolSuggest)
                .expect("tool suggest feature should be configurable");
            approve_account_app_tools(config);
        })
        .build(&server)
        .await?;

    fixture.submit_turn("hello").await?;
    fixture.submit_turn("again").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(
            request.header("authorization").as_deref(),
            Some(accounts.execution_authorization.as_str())
        );
        assert_request_uses_only_account_apps(request, EXECUTION_TOOL_MARKER, CONTROL_TOOL_MARKER);
    }
    let first_input = requests[0].input();
    let second_input = requests[1].input();
    assert_eq!(
        &second_input[..first_input.len()],
        first_input.as_slice(),
        "the second turn must preserve the first request as an exact cacheable prefix"
    );
    assert_eq!(
        requests[0].body_json()["prompt_cache_key"],
        requests[1].body_json()["prompt_cache_key"]
    );
    assert_eq!(
        codex_login::get_active_account_id(home.path(), AuthCredentialsStoreMode::File)?,
        Some(CONTROL_STORED_ACCOUNT_ID.to_string())
    );
    let received_requests = server.received_requests().await.unwrap_or_default();
    let apps_authorizations = apps_authorizations(&received_requests);
    assert!(!apps_authorizations.is_empty());
    assert!(
        apps_authorizations
            .iter()
            .all(|authorization| authorization == &accounts.execution_authorization),
        "startup Apps MCP used non-execution auth: {apps_authorizations:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_401_refresh_updates_apps_tools_without_refreshing_control_auth()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    let accounts = write_accounts(home.as_ref())?;
    let now = chrono::Utc::now();
    codex_core::account_usage::record_usage_limit_hint(
        home.path(),
        CONTROL_STORED_ACCOUNT_ID,
        /*plan*/ None,
        Some(now + chrono::Duration::hours(1)),
        now,
        /*reached_type*/ None,
    )?;
    let refreshed_access_token = "access-execution-refreshed".to_string();
    let refreshed_authorization = format!("Bearer {refreshed_access_token}");

    mount_models(&server, &accounts.execution_authorization).await;
    mount_models(&server, &refreshed_authorization).await;
    mount_account_apps(
        &server,
        vec![
            accounts.execution_authorization.clone(),
            refreshed_authorization.clone(),
        ],
        /*control_tools_enabled*/ false,
        /*execution_tools_enabled*/ true,
    )
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(RefreshExecutionAccountResponder {
            auth_home: home.path().to_path_buf(),
            access_token: refreshed_access_token.clone(),
            stale_authorization: accounts.execution_authorization.clone(),
            refreshed_authorization: refreshed_authorization.clone(),
            calls: Arc::clone(&calls),
        })
        .expect(3)
        .mount(&server)
        .await;
    let apps_base_url = server.uri();
    let fixture = test_codex()
        .with_home(home.clone())
        .with_home_backed_auth_manager()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.auto_switch_accounts_on_rate_limit = true;
            config.model = Some(MODEL.to_string());
            config.chatgpt_base_url = apps_base_url;
            config
                .features
                .enable(Feature::Apps)
                .expect("Apps feature should be configurable");
            config
                .features
                .disable(Feature::ToolSuggest)
                .expect("tool suggest feature should be configurable");
            approve_account_app_tools(config);
        })
        .build(&server)
        .await?;

    fixture.submit_turn("refresh, then use my mail app").await?;

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let received_requests = server.received_requests().await.unwrap_or_default();
    let response_requests = received_requests
        .iter()
        .filter(|request| {
            request.method.as_str() == "POST" && request.url.path() == "/v1/responses"
        })
        .collect::<Vec<_>>();
    let response_authorizations = response_requests
        .iter()
        .filter_map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        response_authorizations,
        vec![
            accounts.execution_authorization.clone(),
            refreshed_authorization.clone(),
            refreshed_authorization.clone(),
        ]
    );
    let prompt_cache_keys = response_requests
        .iter()
        .map(|request| {
            request_body_json(request)
                .expect("responses request should contain JSON")
                .get("prompt_cache_key")
                .cloned()
                .expect("responses request should contain a prompt cache key")
        })
        .collect::<Vec<_>>();
    assert!(
        prompt_cache_keys.windows(2).all(|keys| keys[0] == keys[1]),
        "same-account refresh changed the prompt cache key: {prompt_cache_keys:?}"
    );
    assert_eq!(
        apps_tool_call_authorizations(&received_requests),
        vec![refreshed_authorization]
    );
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
        codex_login::find_account(
            home.path(),
            AuthCredentialsStoreMode::File,
            EXECUTION_STORED_ACCOUNT_ID,
        )?
        .and_then(|account| account.tokens)
        .map(|tokens| tokens.access_token),
        Some(refreshed_access_token)
    );
    assert_eq!(
        codex_login::get_active_account_id(home.path(), AuthCredentialsStoreMode::File)?,
        Some(CONTROL_STORED_ACCOUNT_ID.to_string())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apps_history_keeps_a_stable_prefix_across_turns() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    write_accounts(home.as_ref())?;
    mount_models(&server, CONTROL_AUTHORIZATION).await;
    mount_account_apps(
        &server,
        Vec::new(),
        /*control_tools_enabled*/ true,
        /*execution_tools_enabled*/ false,
    )
    .await;
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-prefix-1"),
                ev_completed("resp-prefix-1"),
            ]),
            sse(vec![
                ev_response_created("resp-prefix-2"),
                ev_completed("resp-prefix-2"),
            ]),
        ],
    )
    .await;
    let apps_base_url = server.uri();
    let fixture = test_codex()
        .with_home(home)
        .with_home_backed_auth_manager()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.model = Some(MODEL.to_string());
            config.chatgpt_base_url = apps_base_url;
            config
                .features
                .enable(Feature::Apps)
                .expect("Apps feature should be configurable");
            config
                .features
                .disable(Feature::ToolSuggest)
                .expect("tool suggest feature should be configurable");
        })
        .build(&server)
        .await?;

    fixture.submit_turn("first turn").await?;
    fixture.submit_turn("second turn").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    let first_body = requests[0].body_json();
    let second_body = requests[1].body_json();
    let first_input = first_body["input"]
        .as_array()
        .expect("first request should contain input");
    let second_input = second_body["input"]
        .as_array()
        .expect("second request should contain input");
    assert!(second_input.len() > first_input.len());
    assert_eq!(first_input.as_slice(), &second_input[..first_input.len()]);
    assert_eq!(
        requests[1]
            .message_input_texts("developer")
            .join("\n")
            .matches("<apps_instructions>")
            .count(),
        1
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_key_transition_never_sends_the_raw_key_to_apps() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    write_accounts(home.as_ref())?;
    let raw_api_key = "sk-api-key-transition";
    let api_key_authorization = format!("Bearer {raw_api_key}");
    mount_models(&server, CONTROL_AUTHORIZATION).await;
    mount_models(&server, &api_key_authorization).await;
    mount_account_apps(
        &server,
        Vec::new(),
        /*control_tools_enabled*/ true,
        /*execution_tools_enabled*/ false,
    )
    .await;
    let delayed_app_call = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse(vec![
            ev_response_created("resp-api-key-1"),
            ev_function_call_with_namespace(
                "control-app-call",
                CONTROL_NAMESPACE,
                LOOKUP_TOOL_NAME,
                "{}",
            ),
            ev_completed("resp-api-key-1"),
        ]))
        .set_delay(Duration::from_secs(2));
    let response_mock = mount_response_sequence(
        &server,
        vec![
            delayed_app_call,
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    ev_assistant_message("msg-api-key-1", "transitioned"),
                    ev_completed("resp-api-key-2"),
                ])),
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    ev_assistant_message("msg-api-key-2", "still safe"),
                    ev_completed("resp-api-key-3"),
                ])),
        ],
    )
    .await;
    let apps_base_url = server.uri();
    let fixture = test_codex()
        .with_home(home.clone())
        .with_home_backed_auth_manager()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.model = Some(MODEL.to_string());
            config.chatgpt_base_url = apps_base_url;
            config
                .features
                .enable(Feature::Apps)
                .expect("Apps feature should be configurable");
            config
                .features
                .disable(Feature::ToolSuggest)
                .expect("tool suggest feature should be configurable");
            approve_account_app_tools(config);
        })
        .build(&server)
        .await?;

    fixture
        .codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "use my calendar app".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    let request_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let received_requests = server.received_requests().await.unwrap_or_default();
        if received_requests
            .iter()
            .any(|request| request.url.path() == "/v1/responses")
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < request_deadline,
            "initial model request was not observed before auth transition"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    codex_login::save_auth(
        home.path(),
        &AuthDotJson {
            auth_mode: Some(codex_app_server_protocol::AuthMode::ApiKey),
            openai_api_key: Some(raw_api_key.to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
        },
        AuthCredentialsStoreMode::File,
    )?;
    codex_login::set_active_account_id(
        home.path(),
        AuthCredentialsStoreMode::File,
        /*account_id*/ None,
    )?;
    fixture
        .thread_manager
        .reload_auth_for_loaded_threads()
        .await;
    wait_for_event(&fixture.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    fixture.submit_turn("continue without apps").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].header("authorization").as_deref(),
        Some(CONTROL_AUTHORIZATION)
    );
    assert_eq!(
        requests[1].header("authorization").as_deref(),
        Some(api_key_authorization.as_str())
    );
    assert_eq!(
        requests[2].header("authorization").as_deref(),
        Some(api_key_authorization.as_str())
    );
    let second_turn_developer_text = requests[2].message_input_texts("developer").join("\n");
    assert!(second_turn_developer_text.contains("state: unavailable"));
    assert_request_has_no_apps_tools(&requests[2]);

    let received_requests = server.received_requests().await.unwrap_or_default();
    let apps_requests = received_requests
        .iter()
        .filter(|request| request.url.path() == "/api/codex/apps")
        .collect::<Vec<_>>();
    assert!(!apps_requests.is_empty());
    assert!(apps_requests.iter().all(|request| {
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            != Some(api_key_authorization.as_str())
    }));
    assert!(apps_requests.iter().any(|request| {
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .is_none()
    }));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_resume_compaction_and_fork_preserve_incremental_apps_context() -> anyhow::Result<()>
{
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    let accounts = write_accounts(home.as_ref())?;
    mount_models(&server, CONTROL_AUTHORIZATION).await;
    mount_models(&server, &accounts.execution_authorization).await;
    mount_account_apps(
        &server,
        vec![accounts.execution_authorization],
        /*control_tools_enabled*/ true,
        /*execution_tools_enabled*/ true,
    )
    .await;
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-source"),
                ev_completed("resp-source"),
            ]),
            sse(vec![
                ev_response_created("resp-resume"),
                ev_completed("resp-resume"),
            ]),
            sse(vec![
                ev_response_created("resp-fork"),
                ev_completed("resp-fork"),
            ]),
        ],
    )
    .await;
    let apps_base_url = server.uri();
    let builder = || {
        let apps_base_url = apps_base_url.clone();
        test_codex()
            .with_home(home.clone())
            .with_home_backed_auth_manager()
            .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
            .with_config(move |config| {
                config.auto_switch_accounts_on_rate_limit = true;
                config.model = Some(MODEL.to_string());
                config.chatgpt_base_url = apps_base_url;
                config
                    .features
                    .enable(Feature::Apps)
                    .expect("Apps feature should be configurable");
                config
                    .features
                    .disable(Feature::ToolSuggest)
                    .expect("tool suggest feature should be configurable");
            })
    };

    let mut source_builder = builder();
    let source = source_builder.build(&server).await?;
    source.submit_turn("source turn").await?;
    let rollout_path = source
        .codex
        .rollout_path()
        .expect("source thread should have a rollout path");
    source.codex.shutdown_and_wait().await?;

    let mut rollout_items = std::fs::read_to_string(&rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let legacy_developer_content = rollout_items
        .iter_mut()
        .find(|item| {
            item.get("type") == Some(&json!("response_item"))
                && item.pointer("/payload/role") == Some(&json!("developer"))
        })
        .and_then(|item| item.pointer_mut("/payload/content"))
        .and_then(Value::as_array_mut)
        .expect("source rollout should contain initial developer context");
    legacy_developer_content.extend([
        json!({ "type": "input_text", "text": "preserved legacy developer context" }),
        json!({
            "type": "input_text",
            "text": "<apps_instructions>legacy apps one</apps_instructions>"
        }),
        json!({
            "type": "input_text",
            "text": "<apps_instructions>legacy apps two</apps_instructions>"
        }),
    ]);
    let mut rewritten_rollout = rollout_items
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    rewritten_rollout.push('\n');
    std::fs::write(&rollout_path, rewritten_rollout)?;

    let mut resumed_builder = builder();
    let resumed = resumed_builder
        .resume(&server, home.clone(), rollout_path.clone())
        .await?;
    let compact_mock =
        mount_compact_user_history_with_summary_once(&server, "legacy apps compacted").await;
    resumed.codex.submit(Op::Compact).await?;
    wait_for_event(&resumed.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let compact_request = compact_mock.single_request();
    let compact_developer_text = compact_request.message_input_texts("developer").join("\n");
    assert_eq!(
        compact_developer_text
            .matches("<apps_instructions>")
            .count(),
        1
    );
    assert!(compact_developer_text.contains("preserved legacy developer context"));
    assert!(!compact_developer_text.contains("legacy apps one"));
    assert!(!compact_developer_text.contains("legacy apps two"));
    resumed.submit_turn("resumed turn").await?;

    let mut fork_config = resumed.config.clone();
    fork_config
        .features
        .disable(Feature::Apps)
        .expect("Apps feature should be configurable");
    let NewThread { thread: forked, .. } = resumed
        .thread_manager
        .fork_thread(
            ForkSnapshot::Interrupted,
            fork_config,
            rollout_path,
            /*thread_source*/ None,
            /*parent_trace*/ None,
        )
        .await?;
    forked
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: "forked turn".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&forked, |event| matches!(event, EventMsg::TurnComplete(_))).await;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    let resumed_developer_text = requests[1].message_input_texts("developer").join("\n");
    assert_eq!(
        resumed_developer_text
            .matches("<apps_instructions>")
            .count(),
        1
    );
    let forked_developer_text = requests[2].message_input_texts("developer").join("\n");
    assert_eq!(
        forked_developer_text.matches("<apps_instructions>").count(),
        1
    );
    assert!(forked_developer_text.contains("<apps_update>"));
    assert!(forked_developer_text.contains("state: unavailable"));
    assert_request_has_no_apps_tools(&requests[2]);

    resumed.codex.shutdown_and_wait().await?;
    forked.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failover_rebuilds_apps_context_without_previous_account_leakage() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    let accounts = write_accounts(home.as_ref())?;
    mount_models(&server, CONTROL_AUTHORIZATION).await;
    mount_models(&server, &accounts.execution_authorization).await;
    mount_account_apps(
        &server,
        vec![accounts.execution_authorization.clone()],
        /*control_tools_enabled*/ false,
        /*execution_tools_enabled*/ true,
    )
    .await;

    let usage_limit_response = ResponseTemplate::new(429).set_body_json(json!({
        "error": {
            "type": "usage_limit_reached",
            "message": "limit reached",
            "resets_at": 1704067242,
            "plan_type": "pro"
        }
    }));
    let tool_response = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse(vec![
            ev_response_created("resp-2"),
            ev_function_call_with_namespace(
                "execution-app-call",
                EXECUTION_NAMESPACE,
                LOOKUP_TOOL_NAME,
                "{}",
            ),
            ev_completed("resp-2"),
        ]));
    let success_response = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse(vec![
            ev_assistant_message("msg-1", "recovered"),
            ev_completed("resp-3"),
        ]));
    let response_mock = mount_response_sequence(
        &server,
        vec![usage_limit_response, tool_response, success_response],
    )
    .await;
    let apps_base_url = server.uri();
    let fixture = test_codex()
        .with_home(home.clone())
        .with_home_backed_auth_manager()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.auto_switch_accounts_on_rate_limit = true;
            config.model = Some(MODEL.to_string());
            config.chatgpt_base_url = apps_base_url;
            config
                .features
                .enable(Feature::Apps)
                .expect("Apps feature should be configurable");
            config
                .features
                .disable(Feature::ToolSuggest)
                .expect("tool suggest feature should be configurable");
            approve_account_app_tools(config);
        })
        .build(&server)
        .await?;

    fixture.submit_turn("hello").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].header("authorization").as_deref(),
        Some(CONTROL_AUTHORIZATION)
    );
    assert_request_has_no_apps(&requests[0]);
    assert_eq!(
        requests[1].header("authorization").as_deref(),
        Some(accounts.execution_authorization.as_str())
    );
    assert_request_uses_only_account_apps(&requests[1], EXECUTION_TOOL_MARKER, CONTROL_TOOL_MARKER);
    assert_eq!(
        requests[2].header("authorization").as_deref(),
        Some(accounts.execution_authorization.as_str())
    );
    assert_request_uses_only_account_apps(&requests[2], EXECUTION_TOOL_MARKER, CONTROL_TOOL_MARKER);
    assert_eq!(
        codex_login::get_active_account_id(home.path(), AuthCredentialsStoreMode::File)?,
        Some(CONTROL_STORED_ACCOUNT_ID.to_string())
    );
    let received_requests = server.received_requests().await.unwrap_or_default();
    let apps_authorizations = apps_authorizations(&received_requests);
    assert!(
        apps_authorizations
            .iter()
            .any(|authorization| authorization == CONTROL_AUTHORIZATION),
        "initial Apps MCP never used the control/execution account: {apps_authorizations:?}"
    );
    assert!(
        apps_authorizations
            .iter()
            .any(|authorization| authorization == &accounts.execution_authorization),
        "failover Apps MCP never refreshed to execution auth: {apps_authorizations:?}"
    );
    assert_eq!(
        apps_tool_call_authorizations(&received_requests),
        vec![accounts.execution_authorization]
    );

    Ok(())
}
