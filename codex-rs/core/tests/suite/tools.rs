#![cfg(not(target_os = "windows"))]
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

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
use codex_core::sandboxing::SandboxPermissions;
use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use core_test_support::assert_regex_match;
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
use core_test_support::skip_if_sandbox;
use core_test_support::test_codex::local;
use core_test_support::test_codex::test_codex;
use regex_lite::Regex;
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
    assert_eq!(
        (
            strip_response_item_ids_from_json(Value::Array(custom_tool_calls)),
            strip_response_item_ids_from_json(request.custom_tool_call_output(call_id)),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_command_escalated_permissions_rejected_then_ok() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_model("test-gpt-5-codex");
    let test = builder.build(&server).await?;

    let command = "echo shell ok";
    let call_id_blocked = "shell-command-blocked";
    let call_id_success = "shell-command-success";

    let first_args = json!({
        "command": command,
        "login": false,
        "timeout_ms": 1_000,
        "sandbox_permissions": SandboxPermissions::RequireEscalated,
    });
    let second_args = json!({
        "command": command,
        "login": false,
        "timeout_ms": 1_000,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(
                call_id_blocked,
                "shell_command",
                &serde_json::to_string(&first_args)?,
            ),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let second_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-2"),
            ev_function_call(
                call_id_success,
                "shell_command",
                &serde_json::to_string(&second_args)?,
            ),
            ev_completed("resp-2"),
        ]),
    )
    .await;
    let third_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-3"),
        ]),
    )
    .await;

    test.submit_turn_with_approval_and_permission_profile(
        "run the shell_command script",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let policy = AskForApproval::Never;
    let expected_message = format!(
        "approval policy is {policy:?}; reject command — you should not ask for escalated permissions if the approval policy is {policy:?}"
    );

    let blocked_output = second_mock
        .single_request()
        .function_call_output_content_and_success(call_id_blocked)
        .and_then(|(content, _)| content)
        .expect("blocked output string");
    assert_eq!(
        blocked_output, expected_message,
        "unexpected rejection message"
    );

    let success_output = third_mock
        .single_request()
        .function_call_output_content_and_success(call_id_success)
        .and_then(|(content, _)| content)
        .expect("success output string");
    assert_regex_match(
        r"(?s)^Exit code: 0\nWall time: [0-9]+(?:\.[0-9]+)? seconds\nOutput:\nshell ok\n?$",
        &success_output,
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sandbox_denied_shell_command_returns_original_output() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_model("gpt-5.4");
    let fixture = builder.build(&server).await?;

    let call_id = "sandbox-denied-shell-command";
    let target_path = fixture.workspace_path("sandbox-denied.txt");
    let sentinel = "sandbox-denied sentinel output";
    let command = format!(
        "printf {sentinel:?}; printf {content:?} > {path:?}",
        sentinel = format!("{sentinel}\n"),
        content = "sandbox denied",
        path = &target_path
    );
    let args = json!({
        "command": command,
        "login": false,
        "timeout_ms": 5_000,
    });

    let responses = vec![
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    ];
    let mock = mount_sse_sequence(&server, responses).await;

    fixture
        .submit_turn_with_permission_profile(
            "run a command that should be denied by the read-only sandbox",
            PermissionProfile::read_only(),
        )
        .await?;

    let output_text = mock
        .function_call_output_text(call_id)
        .context("shell output present")?;
    let exit_code_line = output_text
        .lines()
        .next()
        .context("exit code line present")?;
    let exit_code = exit_code_line
        .strip_prefix("Exit code: ")
        .context("exit code prefix present")?
        .trim()
        .parse::<i32>()
        .context("exit code is integer")?;
    let body = output_text;

    let body_lower = body.to_lowercase();
    // Required for multi-OS.
    let has_denial = body_lower.contains("permission denied")
        || body_lower.contains("operation not permitted")
        || body_lower.contains("read-only file system");
    assert!(
        has_denial,
        "expected sandbox denial details in tool output: {body}"
    );
    assert!(
        body.contains(sentinel),
        "expected sentinel output from command to reach the model: {body}"
    );
    let target_path_str = target_path
        .to_str()
        .context("target path string representation")?;
    assert!(
        body.contains(target_path_str),
        "expected sandbox error to mention denied path: {body}"
    );
    assert!(
        !body_lower.contains("failed in sandbox"),
        "expected original tool output, found fallback message: {body}"
    );
    assert_ne!(
        exit_code, 0,
        "sandbox denial should surface a non-zero exit code"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_command_enforces_glob_deny_read_policy() -> Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex()
        .with_model("gpt-5.4")
        .with_config(move |config| {
            let mut file_system_sandbox_policy = FileSystemSandboxPolicy::default();
            file_system_sandbox_policy
                .entries
                .push(FileSystemSandboxEntry {
                    path: FileSystemPath::GlobPattern {
                        pattern: format!("{}/**/*.env", config.cwd.as_path().display()),
                    },
                    access: FileSystemAccessMode::Deny,
                    missing_path_behavior: None,
                });
            config
                .permissions
                .set_permission_profile(PermissionProfile::from_runtime_permissions(
                    &file_system_sandbox_policy,
                    NetworkSandboxPolicy::Restricted,
                ))
                .expect("set permission profile");
        });
    let fixture = builder.build(&server).await?;

    let fixture_dir = fixture.workspace_path("glob-deny-read");
    fs::create_dir_all(&fixture_dir).context("create glob deny-read fixture directory")?;
    let denied_path = fixture_dir.join("secret.env");
    let allowed_path = fixture_dir.join("notes.txt");
    let secret = "shell glob deny-read secret";
    let allowed = "shell glob deny-read allowed";
    fs::write(&denied_path, format!("{secret}\n")).context("write denied fixture")?;
    fs::write(&allowed_path, format!("{allowed}\n")).context("write allowed fixture")?;

    let call_id = "shell-command-glob-deny-read";
    let command = format!(
        "rc=0; cat {denied_path:?} || rc=$?; cat {allowed_path:?}; exit \"$rc\"",
        denied_path = denied_path.to_string_lossy(),
        allowed_path = allowed_path.to_string_lossy(),
    );
    let args = json!({
        "command": command,
        "login": false,
        "timeout_ms": 1_000,
    });

    let responses = vec![
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    ];
    let mock = mount_sse_sequence(&server, responses).await;

    let permission_profile = fixture.session_configured.permission_profile.clone();
    fixture
        .submit_turn_with_permission_profile("read the fixture files", permission_profile)
        .await?;

    let output_text = mock
        .function_call_output_text(call_id)
        .context("shell output present")?;
    let exit_code_line = output_text
        .lines()
        .next()
        .context("exit code line present")?;
    let exit_code = exit_code_line
        .strip_prefix("Exit code: ")
        .context("exit code prefix present")?
        .trim()
        .parse::<i32>()
        .context("exit code is integer")?;

    assert_ne!(
        exit_code, 0,
        "glob deny-read should surface a non-zero exit code"
    );
    assert!(
        output_text.contains(allowed),
        "expected allowed file contents in shell output: {output_text}"
    );
    assert!(
        !output_text.contains(secret),
        "denied file contents leaked into shell output: {output_text}"
    );
    let output_lower = output_text.to_lowercase();
    let has_denial = output_lower.contains("permission denied")
        || output_lower.contains("operation not permitted")
        || output_lower.contains("read-only file system");
    assert!(
        has_denial,
        "expected sandbox denial details in shell output: {output_text}"
    );

    Ok(())
}

async fn collect_tools(use_unified_exec: bool) -> Result<Vec<String>> {
    let server = start_mock_server().await;

    let responses = vec![sse(vec![
        ev_response_created("resp-1"),
        ev_assistant_message("msg-1", "done"),
        ev_completed("resp-1"),
    ])];
    let mock = mount_sse_sequence(&server, responses).await;

    let mut builder = test_codex().with_config(move |config| {
        if use_unified_exec {
            config
                .features
                .enable(Feature::UnifiedExec)
                .expect("test config should allow feature update");
        } else {
            config
                .features
                .disable(Feature::UnifiedExec)
                .expect("test config should allow feature update");
        }
    });
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

    let tools_disabled = collect_tools(/*use_unified_exec*/ false).await?;
    assert!(
        !tools_disabled.iter().any(|name| name == "exec_command"),
        "tools list should not include exec_command when disabled: {tools_disabled:?}"
    );
    assert!(
        !tools_disabled.iter().any(|name| name == "write_stdin"),
        "tools list should not include write_stdin when disabled: {tools_disabled:?}"
    );

    let tools_enabled = collect_tools(/*use_unified_exec*/ true).await?;
    assert!(
        tools_enabled.iter().any(|name| name == "exec_command"),
        "tools list should include exec_command when enabled: {tools_enabled:?}"
    );
    assert!(
        tools_enabled.iter().any(|name| name == "write_stdin"),
        "tools list should include write_stdin when enabled: {tools_enabled:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_command_timeout_includes_timeout_prefix_and_metadata() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_model("test-gpt-5-codex");
    let test = builder.build(&server).await?;

    let call_id = "shell-command-timeout";
    let timeout_ms = 50u64;
    let args = json!({
        "command": "yes line | head -n 400; sleep 1",
        "login": false,
        "timeout_ms": timeout_ms,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let second_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn_with_approval_and_permission_profile(
        "run a long command",
        AskForApproval::Never,
        PermissionProfile::Disabled,
    )
    .await?;

    let timeout_item = second_mock.single_request().function_call_output(call_id);

    let output_str = timeout_item
        .get("output")
        .and_then(Value::as_str)
        .expect("timeout output string");

    // The exec path can report a timeout in two ways depending on timing:
    // 1) Structured JSON with exit_code 124 and a timeout prefix (preferred), or
    // 2) A plain error string if the child is observed as killed by a signal first.
    if let Ok(output_json) = serde_json::from_str::<Value>(output_str) {
        assert_eq!(
            output_json["metadata"]["exit_code"].as_i64(),
            Some(124),
            "expected timeout exit code 124",
        );

        let stdout = output_json["output"].as_str().unwrap_or_default();
        assert!(
            stdout.contains("command timed out"),
            "timeout output missing `command timed out`: {stdout}"
        );
    } else {
        let normalized_output = output_str
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .trim_end_matches('\n')
            .to_string();

        let shell_output_pattern = r"(?s)^Exit code: 124\nWall time: [0-9]+(?:\.[0-9]+)? seconds\nOutput:\ncommand timed out after [0-9]+ milliseconds\n(?:.*)?$";
        if Regex::new(shell_output_pattern)
            .expect("shell timeout output regex should compile")
            .is_match(&normalized_output)
        {
            return Ok(());
        }

        // Fallback: accept the signal classification path to deflake the test.
        let signal_pattern = r"(?is)^execution error:.*signal.*$";
        assert_regex_match(signal_pattern, output_str);
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_command_timeout_handles_background_grandchild_stdout() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mut builder = test_codex().with_model("gpt-5.4").with_config(|config| {
        config
            .permissions
            .set_permission_profile(PermissionProfile::Disabled)
            .expect("set permission profile");
    });
    let test = builder.build(&server).await?;

    let call_id = "shell-command-grandchild-timeout";
    let pid_path = test.cwd.path().join("grandchild_pid.txt");
    let script_path = test.cwd.path().join("spawn_detached.py");
    let script = format!(
        r#"import subprocess
import time
from pathlib import Path

# Spawn a detached grandchild that inherits stdout/stderr so the pipe stays open.
proc = subprocess.Popen(["/bin/sh", "-c", "sleep 60"], start_new_session=True)
Path({pid_path:?}).write_text(str(proc.pid))
time.sleep(60)
"#
    );
    fs::write(&script_path, script)?;

    let args = json!({
        "command": format!("python3 {:?}", script_path.to_string_lossy()),
        "login": false,
        "timeout_ms": 200,
    });

    mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_function_call(call_id, "shell_command", &serde_json::to_string(&args)?),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let second_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    let start = Instant::now();
    let output_str = tokio::time::timeout(Duration::from_secs(10), async {
        test.submit_turn_with_approval_and_permission_profile(
            "run a command with a detached grandchild",
            AskForApproval::Never,
            PermissionProfile::Disabled,
        )
        .await?;
        let timeout_item = second_mock.single_request().function_call_output(call_id);
        timeout_item
            .get("output")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("timeout output string")
    })
    .await
    .context("exec call should not hang waiting for grandchild pipes to close")??;
    let elapsed = start.elapsed();

    if let Ok(output_json) = serde_json::from_str::<Value>(&output_str) {
        assert_eq!(
            output_json["metadata"]["exit_code"].as_i64(),
            Some(124),
            "expected timeout exit code 124",
        );
    } else {
        let timeout_pattern = r"(?is)command timed out|timeout";
        assert_regex_match(timeout_pattern, &output_str);
    }

    assert!(
        elapsed < Duration::from_secs(9),
        "command should return shortly after timeout even with live grandchildren: {elapsed:?}"
    );

    if let Ok(pid_str) = fs::read_to_string(&pid_path)
        && let Ok(pid) = pid_str.trim().parse::<libc::pid_t>()
    {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }

    Ok(())
}
