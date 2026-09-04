#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use codex_code_bridge_client::CodeBridgeClient;
use codex_code_bridge_client::CodeBridgeEventStream;
use codex_code_bridge_client::CodeBridgeSession;
use codex_code_bridge_client::ControlResponse;
use codex_code_bridge_client::ScreenshotResponse;
use codex_code_bridge_protocol::BridgePayload;
use codex_code_bridge_protocol::CapabilitySet;
use codex_code_bridge_protocol::ClientMetadata;
use codex_code_bridge_protocol::ClientRole;
use codex_code_bridge_protocol::ControlCommand;
use codex_code_bridge_protocol::ControlStatus;
use codex_code_bridge_protocol::ScreenshotMediaType;
use codex_code_bridge_protocol::ScreenshotPayload;
use codex_code_bridge_protocol::SourceKind;
use codex_config::test_support::CloudConfigBundleFixture;
use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::protocol::AskForApproval;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_custom_tool_call_with_namespace;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::responses::strip_response_item_ids_from_json;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::local;
use core_test_support::test_codex::test_codex;
use serde_json::Value;
use serde_json::json;

/// A real, decodable 1x1 PNG. It must be a genuinely valid image (correct chunk CRCs included):
/// the model-visible image pipeline replaces payloads it cannot decode with a text placeholder,
/// which would silently hide the metadata/image separation this test proves.
const SCREENSHOT_FIXTURE_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

fn tool_names(body: &Value) -> Vec<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("name")
                        .or_else(|| tool.get("type"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Spawns a Code Bridge service rooted at the test's `CODEX_LAB_HOME` so the `code_bridge` tool
/// handler discovers the same descriptor the producer fixture registers against.
async fn start_test_bridge(
    codex_home: &Path,
) -> Result<(
    codex_code_bridge_service::BridgeServiceHandle,
    CodeBridgeClient,
)> {
    let bridge = codex_code_bridge_service::start(
        codex_code_bridge_service::BridgeServiceConfig::new(codex_home.to_path_buf()),
    )
    .await?;
    let client = CodeBridgeClient::from_descriptor_path(bridge.descriptor_path())?;
    Ok((bridge, client))
}

async fn producer_session(
    client: &CodeBridgeClient,
    capabilities: CapabilitySet,
) -> Result<(CodeBridgeSession, CodeBridgeEventStream)> {
    let session = client
        .hello(
            "producer-1",
            ClientRole::Producer,
            capabilities,
            ClientMetadata {
                source_kind: SourceKind::TestFixture,
                label: Some("core-suite-producer".to_string()),
                provenance: None,
            },
        )
        .await?;
    let events = client.events(&session, /*last_event_id*/ 0).await?;
    Ok((session, events))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_bridge_screenshot_returns_bounded_metadata_and_image_output() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let call_id = "code-bridge-screenshot";
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    call_id,
                    "code_bridge",
                    r#"{"action":"screenshot","targetClientId":"producer-1","timeoutMs":1000}"#,
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex();
    let test = builder.build(&server).await?;
    let (bridge, bridge_client) = start_test_bridge(test.codex_home_path()).await?;
    let (session, mut events) = producer_session(
        &bridge_client,
        CapabilitySet {
            provide_screenshot: true,
            ..CapabilitySet::default()
        },
    )
    .await?;
    let producer_client = bridge_client.clone();
    let producer_task = tokio::spawn(async move {
        loop {
            let message = events.next_message().await?;
            let BridgePayload::ScreenshotRequest(request) = message.envelope.payload else {
                continue;
            };
            assert_eq!(request.target_client_id, "producer-1");
            producer_client
                .respond_screenshot(
                    &session,
                    ScreenshotResponse {
                        request_id: request.request_id,
                        status: ControlStatus::Ok,
                        screenshot: Some(ScreenshotPayload {
                            width: 1,
                            height: 1,
                            media_type: ScreenshotMediaType::Png,
                            data_base64: SCREENSHOT_FIXTURE_BASE64.to_string(),
                        }),
                        error: None,
                    },
                )
                .await?;
            return anyhow::Ok(());
        }
    });

    test.submit_turn("capture the browser").await?;

    producer_task.await??;
    bridge.shutdown().await;

    let output = responses
        .requests()
        .last()
        .context("follow-up model request should be captured")?
        .function_call_output(call_id)["output"]
        .clone();
    let items = output
        .as_array()
        .context("screenshot output should be structured content")?;

    let metadata_text = items[0]["text"]
        .as_str()
        .context("screenshot metadata text item")?;
    let metadata: Value = serde_json::from_str(metadata_text)?;
    assert_eq!(metadata["status"], "ok");
    assert_eq!(metadata["screenshot"]["mediaType"], "image/png");
    assert_eq!(metadata["screenshot"]["width"], 1);
    assert_eq!(metadata["screenshot"]["height"], 1);
    // Raw pixel data must never ride along in the model-visible text fragment; it is either a
    // separate image item or safely omitted.
    assert!(
        !metadata_text.contains(SCREENSHOT_FIXTURE_BASE64),
        "screenshot base64 must not appear in model-visible metadata text: {metadata_text}"
    );

    // The context-safety lane may omit oversized screenshots instead of inlining them. Accept
    // either bounded outcome, but require the two shapes stay mutually exclusive and described.
    if metadata["screenshot"]["imageOmitted"].is_null() {
        assert_eq!(items.len(), 2);
        assert_eq!(items[1]["type"], "input_image");
        // The prompt image pipeline re-encodes the payload, so assert the transport shape rather
        // than byte equality with the fixture.
        let image_url = items[1]["image_url"].as_str().context("image_url")?;
        assert!(
            image_url.starts_with("data:image/png;base64,"),
            "screenshot image must be an inline png data URL: {image_url}"
        );
        assert!(
            image_url.len() > "data:image/png;base64,".len(),
            "screenshot image data URL must carry a payload: {image_url}"
        );
    } else {
        assert_eq!(items.len(), 1);
        assert!(
            metadata["screenshot"]["imageOmitted"]["reason"].is_string(),
            "omitted screenshots must explain themselves: {metadata}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_bridge_status_tool_is_model_visible_and_bounded_when_unavailable() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let call_id = "code-bridge-status";
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(call_id, "code_bridge", r#"{"action":"status"}"#),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex();
    let test = builder.build(&server).await?;

    // No bridge service is started, so the handler must report the unavailable path.
    test.submit_turn("check Code Bridge status").await?;

    let requests = responses.requests();
    let tools = tool_names(&requests[0].body_json());
    assert!(
        tools.contains(&"code_bridge".to_string()),
        "code_bridge should be visible to the model; got {tools:?}"
    );

    let output = responses
        .function_call_output_text(call_id)
        .context("function_call_output present for code_bridge status")?;
    let output: Value = serde_json::from_str(&output)?;
    assert_eq!(output["status"], "unavailable");
    let message = output["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains(test.codex_home_path().to_string_lossy().as_ref()),
        "status output should not expose local descriptor path: {message}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_cdp_is_denied_without_full_access() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let call_id = "browser-cdp-denied";
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    call_id,
                    "browser",
                    r#"{"action":"cdp","method":"Page.getFrameTree"}"#,
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex();
    builder = builder.with_config(|config| {
        let _ = config.features.enable(Feature::InAppBrowser);
        let _ = config.features.enable(Feature::BrowserUse);
        let _ = config.features.disable(Feature::BrowserUseFullCdpAccess);
    });
    let test = builder.build(&server).await?;

    test.submit_turn("run a browser CDP command").await?;

    let requests = responses.requests();
    assert!(
        tool_names(&requests[0].body_json()).contains(&"browser".to_string()),
        "browser should be visible to the model"
    );
    let output = responses
        .function_call_output_text(call_id)
        .context("function_call_output present for browser CDP denial")?;
    let output: Value = serde_json::from_str(&output)?;
    assert_eq!(output["status"], "failed");
    assert_eq!(
        output["error"]["message"],
        "CDP access is disabled by browser-use policy"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_bridge_javascript_returns_bounded_control_output() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let call_id = "code-bridge-javascript";
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    call_id,
                    "code_bridge",
                    r#"{"action":"javascript","targetClientId":"producer-1","code":"window.location.href","timeoutMs":1000}"#,
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_assistant_message("msg-1", "done"),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex();
    let test = builder.build(&server).await?;
    let (bridge, bridge_client) = start_test_bridge(test.codex_home_path()).await?;
    let (session, mut events) = producer_session(
        &bridge_client,
        CapabilitySet {
            provide_control: true,
            provide_javascript_execution: true,
            ..CapabilitySet::default()
        },
    )
    .await?;
    let producer_client = bridge_client.clone();
    let producer_task = tokio::spawn(async move {
        loop {
            let message = events.next_message().await?;
            let BridgePayload::ControlRequest(request) = message.envelope.payload else {
                continue;
            };
            assert_eq!(request.target_client_id, "producer-1");
            assert_eq!(
                request.command,
                ControlCommand::ExecuteJavascript {
                    code: "window.location.href".to_string()
                }
            );
            producer_client
                .respond_control_with_result(
                    &session,
                    ControlResponse {
                        request_id: request.request_id,
                        status: ControlStatus::Ok,
                        summary: "javascript completed".to_string(),
                        error: None,
                    },
                    json!({ "href": "https://example.test/" }),
                )
                .await?;
            return anyhow::Ok(());
        }
    });

    test.submit_turn("inspect the browser location").await?;

    producer_task.await??;
    bridge.shutdown().await;

    let output = responses
        .function_call_output_text(call_id)
        .context("function_call_output present for code_bridge javascript")?;
    let output: Value = serde_json::from_str(&output)?;
    assert_eq!(output["status"], "ok");
    assert_eq!(output["summary"], "javascript completed");
    assert_eq!(output["result"], json!({ "href": "https://example.test/" }));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_turn_environments_omits_environment_backed_tools() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.update_plan_enabled = true;
        config
            .features
            .enable(Feature::UnifiedExec)
            .expect("unified exec should enable for test");
    });
    let test = builder.build(&server).await?;

    test.submit_turn_with_environments("which tools are available?", Some(vec![]))
        .await?;

    let tools = tool_names(&response_mock.single_request().body_json());
    assert!(
        tools.contains(&"update_plan".to_string()),
        "non-environment tool should remain available; got {tools:?}"
    );
    for environment_tool in ["exec_command", "write_stdin", "apply_patch", "view_image"] {
        assert!(
            !tools.contains(&environment_tool.to_string()),
            "{environment_tool} should be omitted for explicit empty turn environments; got {tools:?}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_environment_selection_keeps_environment_backed_tools() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config
            .features
            .enable(Feature::UnifiedExec)
            .expect("unified exec should enable for test");
    });
    let test = builder.build(&server).await?;

    test.submit_turn_with_environments(
        "which tools are available?",
        Some(vec![local(test.config.cwd.clone())]),
    )
    .await?;

    let tools = tool_names(&response_mock.single_request().body_json());
    assert!(
        tools.contains(&"exec_command".to_string()),
        "environment tool should remain available with selected local environment; got {tools:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_tool_unknown_returns_custom_output_error() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    let test = builder.build(&server).await?;

    let call_id = "custom-unsupported";
    let tool_name = "unsupported_tool";

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_custom_tool_call(call_id, tool_name, "\"payload\""),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn_with_approval_and_permission_profile(
        "invoke custom tool",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let item = mock.single_request().custom_tool_call_output(call_id);
    let output = item
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected = format!("unsupported custom tool call: {tool_name}");
    assert_eq!(output, expected);
    assert!(
        item.pointer("/internal_chat_message_metadata_passthrough/executed_tool_calls")
            .is_none(),
        "attempted-tool metadata must be disabled by default",
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn namespaced_custom_tool_call_preserves_namespace_through_dispatch_and_replay() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex();
    builder = builder.with_config(|config| {
        let _ = config.features.enable(Feature::ExecutedToolCallMetadata);
    });
    let test = builder.build(&server).await?;

    let call_id = "custom-namespaced";
    let namespace = "test_namespace::";
    let tool_name = "unsupported_tool";
    let input = "\"payload\"";

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_custom_tool_call_with_namespace(call_id, namespace, tool_name, input),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn_with_approval_and_permission_profile(
        "invoke namespaced custom tool",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let request = mock.single_request();
    let custom_tool_calls = request.inputs_of_type("custom_tool_call");
    let turn_id = custom_tool_calls
        .first()
        .and_then(|item| item.pointer("/internal_chat_message_metadata_passthrough/turn_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .expect("custom tool call should include turn metadata");
    let mut custom_tool_output = request.custom_tool_call_output(call_id);
    let output_create_time = custom_tool_output
        .pointer_mut("/internal_chat_message_metadata_passthrough")
        .and_then(Value::as_object_mut)
        .and_then(|metadata| metadata.remove("create_time"))
        .and_then(|create_time| create_time.as_f64())
        .expect("custom tool output should include a creation timestamp");
    assert!(output_create_time > 0.0);
    assert_eq!(
        (
            strip_response_item_ids_from_json(Value::Array(custom_tool_calls)),
            strip_response_item_ids_from_json(custom_tool_output),
        ),
        (
            Value::Array(vec![json!({
                "type": "custom_tool_call",
                "call_id": call_id,
                "namespace": namespace,
                "name": tool_name,
                "input": input,
                "internal_chat_message_metadata_passthrough": {
                    "turn_id": turn_id,
                },
            })]),
            json!({
                "type": "custom_tool_call_output",
                "call_id": call_id,
                "output": format!("unsupported custom tool call: {namespace}{tool_name}"),
                "internal_chat_message_metadata_passthrough": {
                    "turn_id": turn_id,
                    "executed_tool_calls": [{
                        "name": format!("{namespace}__{tool_name}"),
                        "arguments": input,
                    }],
                },
            }),
        )
    );
    let escaped_call_id = "custom-namespaced-escaped";
    let escaped_input = "\\".repeat(4_096);
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-3"),
            ev_custom_tool_call_with_namespace(
                escaped_call_id,
                namespace,
                tool_name,
                &escaped_input,
            ),
            ev_completed("resp-3"),
        ]),
    )
    .await;
    let escaped_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-2", "done"),
            ev_completed("resp-4"),
        ]),
    )
    .await;
    test.submit_turn_with_approval_and_permission_profile(
        "invoke namespaced custom tool with escaped arguments",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;
    assert_eq!(
        escaped_mock
            .single_request()
            .custom_tool_call_output(escaped_call_id)["internal_chat_message_metadata_passthrough"]
            ["executed_tool_calls"],
        json!([{
            "name": format!("{namespace}__{tool_name}"),
            "arguments": {
                "_codex_executed_tool_call_truncated": {
                    "original_bytes": serde_json::to_vec(&escaped_input)?.len(),
                    "max_bytes": 8 * 1024,
                },
            },
        }]),
    );

    let direct_exec_call_id = "custom-direct-exec";
    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-5"),
            ev_custom_tool_call(
                direct_exec_call_id,
                codex_code_mode::PUBLIC_TOOL_NAME,
                input,
            ),
            ev_completed("resp-5"),
        ]),
    )
    .await;
    let direct_exec_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-3", "done"),
            ev_completed("resp-6"),
        ]),
    )
    .await;

    test.submit_turn_with_approval_and_permission_profile(
        "invoke direct custom exec outside code mode",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let direct_exec_output = direct_exec_mock
        .single_request()
        .custom_tool_call_output(direct_exec_call_id);
    assert_eq!(
        direct_exec_output["output"],
        json!("unsupported custom tool call: exec"),
    );
    assert_eq!(
        direct_exec_output["internal_chat_message_metadata_passthrough"]["executed_tool_calls"],
        json!([{
            "name": codex_code_mode::PUBLIC_TOOL_NAME,
            "arguments": input,
        }]),
    );

    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum CommandToolAvailability {
    Default,
    ManagedUnifiedExecDisabled,
    ShellToolDisabled,
    ModelDisabled,
}

async fn collect_tools(availability: CommandToolAvailability) -> Result<Vec<String>> {
    let server = start_mock_server().await;

    let responses = vec![sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-1"),
    ])];
    let mock = mount_sse_sequence(&server, responses).await;

    let mut builder = match availability {
        CommandToolAvailability::Default => test_codex(),
        CommandToolAvailability::ManagedUnifiedExecDisabled => test_codex()
            .with_cloud_config_bundle(
                CloudConfigBundleFixture::loader_with_enterprise_requirement(
                    r#"
[features]
unified_exec = false
shell_tool = true
"#,
                ),
            ),
        CommandToolAvailability::ShellToolDisabled => test_codex().with_config(|config| {
            config
                .features
                .disable(Feature::ShellTool)
                .expect("test config should allow feature update");
        }),
        CommandToolAvailability::ModelDisabled => {
            test_codex().with_model_info_override("gpt-5.4", |model| {
                model.shell_type = ConfigShellToolType::Disabled;
            })
        }
    };
    let test = builder.build(&server).await?;

    test.submit_turn_with_approval_and_permission_profile(
        "list tools",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let first_body = mock.single_request().body_json();
    Ok(tool_names(&first_body))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_exec_spec_toggle_end_to_end() -> Result<()> {
    skip_if_no_network!(Ok(()));

    for availability in [
        CommandToolAvailability::ShellToolDisabled,
        CommandToolAvailability::ModelDisabled,
    ] {
        let tools = collect_tools(availability).await?;
        for command_tool in ["exec_command", "write_stdin"] {
            assert!(
                !tools.iter().any(|name| name == command_tool),
                "tools list should not include {command_tool} for {availability:?}: {tools:?}"
            );
        }
    }

    for availability in [CommandToolAvailability::Default] {
        let tools = collect_tools(availability).await?;
        for command_tool in ["exec_command", "write_stdin"] {
            assert!(
                tools.iter().any(|name| name == command_tool),
                "tools list should include {command_tool} for {availability:?}: {tools:?}"
            );
        }
    }

    let tools = collect_tools(CommandToolAvailability::ManagedUnifiedExecDisabled).await?;
    assert!(
        tools.iter().any(|name| name == "exec_command"),
        "managed unified-exec disable should keep one-shot command execution: {tools:?}"
    );
    assert!(
        !tools.iter().any(|name| name == "write_stdin"),
        "managed unified-exec disable must not expose retained process authority: {tools:?}"
    );

    Ok(())
}
