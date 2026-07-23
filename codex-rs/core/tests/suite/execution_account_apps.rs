use base64::Engine as _;
use codex_config::types::AuthCredentialsStoreMode;
use codex_features::Feature;
use codex_login::CodexAuth;
use codex_login::StoredAccount;
use codex_login::TokenData;
use codex_login::token_data::IdTokenInfo;
use codex_models_manager::bundled_models_response;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
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
const CONTROL_TOOL_MARKER: &str = "CONTROL_ACCOUNT_ONLY_TOOL";
const EXECUTION_STORED_ACCOUNT_ID: &str = "stored-1-execution";
const EXECUTION_CHATGPT_ACCOUNT_ID: &str = "execution-account";
const EXECUTION_CONNECTOR_NAME: &str = "Execution Mail";
const EXECUTION_TOOL_MARKER: &str = "EXECUTION_ACCOUNT_ONLY_TOOL";
const MODEL: &str = "gpt-5.4";

#[derive(Clone)]
struct AccountAppsResponder {
    execution_authorization: String,
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
        } else if authorization == self.execution_authorization {
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
        access_token: format!("access-{account_id}"),
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
    execution_authorization: String,
    control_tools_enabled: bool,
    execution_tools_enabled: bool,
) {
    Mock::given(method("POST"))
        .and(path_regex("^/api/codex/apps/?$"))
        .respond_with(AccountAppsResponder {
            execution_authorization,
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
    assert_eq!(
        developer_text.matches("<apps_instructions>").count(),
        1,
        "expected exactly one current-account Apps instructions block: {developer_text}"
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
        accounts.execution_authorization.clone(),
        /*control_tools_enabled*/ false,
        /*execution_tools_enabled*/ true,
    )
    .await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
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
        })
        .build(&server)
        .await?;

    fixture.submit_turn("hello").await?;

    let request = response_mock.single_request();
    assert_eq!(
        request.header("authorization").as_deref(),
        Some(accounts.execution_authorization.as_str())
    );
    assert_request_uses_only_account_apps(&request, EXECUTION_TOOL_MARKER, CONTROL_TOOL_MARKER);
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
async fn failover_rebuilds_apps_context_without_previous_account_leakage() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    let accounts = write_accounts(home.as_ref())?;
    mount_models(&server, CONTROL_AUTHORIZATION).await;
    mount_models(&server, &accounts.execution_authorization).await;
    mount_account_apps(
        &server,
        accounts.execution_authorization.clone(),
        /*control_tools_enabled*/ true,
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
    let success_response = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse(vec![
            ev_assistant_message("msg-1", "recovered"),
            ev_completed("resp-2"),
        ]));
    let response_mock =
        mount_response_sequence(&server, vec![usage_limit_response, success_response]).await;
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
        })
        .build(&server)
        .await?;

    fixture.submit_turn("hello").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].header("authorization").as_deref(),
        Some(CONTROL_AUTHORIZATION)
    );
    assert_request_uses_only_account_apps(&requests[0], CONTROL_TOOL_MARKER, EXECUTION_TOOL_MARKER);
    assert_eq!(
        requests[1].header("authorization").as_deref(),
        Some(accounts.execution_authorization.as_str())
    );
    assert_request_uses_only_account_apps(&requests[1], EXECUTION_TOOL_MARKER, CONTROL_TOOL_MARKER);
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

    Ok(())
}
