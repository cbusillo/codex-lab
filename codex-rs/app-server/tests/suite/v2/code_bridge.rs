use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use codex_app_server_protocol::CodeBridgeAvailability;
use codex_app_server_protocol::CodeBridgeControlStatus;
use codex_app_server_protocol::CodeBridgeErrorCode;
use codex_app_server_protocol::CodeBridgeJavascriptResponse;
use codex_app_server_protocol::CodeBridgeRequestStatus;
use codex_app_server_protocol::CodeBridgeScreenshotMediaType;
use codex_app_server_protocol::CodeBridgeScreenshotPayload;
use codex_app_server_protocol::CodeBridgeScreenshotResponse;
use codex_app_server_protocol::CodeBridgeStatusReadResponse;
use codex_app_server_protocol::CodeBridgeSubscribeResponse;
use codex_app_server_protocol::CodeBridgeUnavailableReason;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_code_bridge_client::CodeBridgeClient;
use codex_code_bridge_client::CodeBridgeEventStream;
use codex_code_bridge_client::CodeBridgeSession;
use codex_code_bridge_client::ControlResponse;
use codex_code_bridge_client::ScreenshotResponse;
use codex_code_bridge_protocol::BridgePayload;
use codex_code_bridge_protocol::BridgeSseMessage;
use codex_code_bridge_protocol::CapabilitySet;
use codex_code_bridge_protocol::ClientMetadata;
use codex_code_bridge_protocol::ClientRole;
use codex_code_bridge_protocol::ControlCommand;
use codex_code_bridge_protocol::ControlRequestMessage;
use codex_code_bridge_protocol::ControlStatus;
use codex_code_bridge_protocol::PROTOCOL_VERSION;
use codex_code_bridge_protocol::ScreenshotMediaType;
use codex_code_bridge_protocol::ScreenshotPayload;
use codex_code_bridge_protocol::ScreenshotRequestMessage;
use codex_code_bridge_protocol::SourceKind;
use codex_code_bridge_service::BridgeServiceConfig;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde::de::DeserializeOwned;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn code_bridge_status_read_reports_missing_descriptor() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let request_id = app_server.send_code_bridge_status_read_request().await?;
    let received: CodeBridgeStatusReadResponse = response_for(&mut app_server, request_id).await?;

    assert_eq!(received.status, CodeBridgeAvailability::Unavailable);
    assert!(!received.control_available);
    assert_eq!(
        received.unavailable_reason,
        Some(CodeBridgeUnavailableReason::DescriptorMissing)
    );
    assert_eq!(received.service, None);
    Ok(())
}

#[tokio::test]
async fn code_bridge_status_read_reports_running_service() -> Result<()> {
    let codex_home = TempDir::new()?;
    let service =
        codex_code_bridge_service::start(BridgeServiceConfig::new(codex_home.path().to_path_buf()))
            .await?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let request_id = app_server.send_code_bridge_status_read_request().await?;
    let received: CodeBridgeStatusReadResponse = response_for(&mut app_server, request_id).await?;

    assert_eq!(received.status, CodeBridgeAvailability::Available);
    assert!(received.control_available);
    assert_eq!(received.unavailable_reason, None);
    let bridge_status = received.service.expect("service status");
    assert_eq!(bridge_status.protocol_version, PROTOCOL_VERSION);
    assert_eq!(bridge_status.connected_producer_count, 0);
    assert_eq!(bridge_status.connected_subscriber_count, 0);

    service.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn code_bridge_status_read_reports_workspace_metadata_bridge() -> Result<()> {
    let codex_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    let nested = workspace.path().join("packages/app");
    std::fs::create_dir_all(&nested)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_addr = listener.local_addr()?;
    let code_dir = workspace.path().join(".code");
    std::fs::create_dir_all(&code_dir)?;
    std::fs::write(
        code_dir.join("code-bridge.json"),
        serde_json::json!({
            "url": format!("ws://{local_addr}"),
            "secret": "workspace-secret",
            "port": local_addr.port(),
            "workspacePath": workspace.path(),
        })
        .to_string(),
    )?;
    let accept_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept bridge client");
        let mut ws = accept_async(stream).await.expect("accept websocket");
        while let Some(message) = ws.next().await {
            let Message::Text(text) = message.expect("websocket message") else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(&text).expect("auth json");
            assert_eq!(
                value.get("type").and_then(|value| value.as_str()),
                Some("auth")
            );
            assert_eq!(
                value.get("secret").and_then(|value| value.as_str()),
                Some("workspace-secret")
            );
            ws.send(Message::Text(
                serde_json::json!({ "type": "auth_success" })
                    .to_string()
                    .into(),
            ))
            .await
            .expect("auth success");
            break;
        }
    });
    let mut app_server = TestAppServer::new_with_cwd(codex_home.path(), &nested).await?;
    timeout(DEFAULT_TIMEOUT, app_server.initialize()).await??;

    let request_id = app_server.send_code_bridge_status_read_request().await?;
    let received: CodeBridgeStatusReadResponse = response_for(&mut app_server, request_id).await?;

    assert_eq!(received.status, CodeBridgeAvailability::Available);
    assert!(!received.control_available);
    assert_eq!(received.unavailable_reason, None);
    assert_eq!(received.service, None);
    accept_task.await?;
    Ok(())
}

#[tokio::test]
async fn code_bridge_subscribe_accepts_without_opening_orphan_session() -> Result<()> {
    let codex_home = TempDir::new()?;
    let service =
        codex_code_bridge_service::start(BridgeServiceConfig::new(codex_home.path().to_path_buf()))
            .await?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let received: CodeBridgeSubscribeResponse = jsonrpc_response(
        &mut app_server,
        "codeBridge/subscribe",
        json!({
            "filter": {
                "levels": ["info"],
                "eventKinds": ["console"],
                "clientIds": ["producer-1"],
            }
        }),
    )
    .await?;

    assert_eq!(
        received,
        CodeBridgeSubscribeResponse {
            status: CodeBridgeRequestStatus::Accepted,
        }
    );
    let status = client_status(service.descriptor_path()).await?;
    assert_eq!(status.connected_subscriber_count, 0);

    service.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn code_bridge_screenshot_round_trips_through_jsonrpc() -> Result<()> {
    let codex_home = TempDir::new()?;
    let service =
        codex_code_bridge_service::start(BridgeServiceConfig::new(codex_home.path().to_path_buf()))
            .await?;
    let mut app_server = initialized_app_server(&codex_home).await?;
    let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())?;
    let (producer, mut producer_events) = producer_session(
        &client,
        CapabilitySet {
            provide_screenshot: true,
            ..CapabilitySet::default()
        },
    )
    .await?;

    let app_request_id = app_server
        .send_raw_request(
            "codeBridge/screenshot",
            Some(json!({
                "targetClientId": "producer-1",
                "timeoutMs": 1_000,
            })),
        )
        .await?;
    let message = next_bridge_message(&mut producer_events, "screenshot request").await;
    let BridgePayload::ScreenshotRequest(ScreenshotRequestMessage { request_id, .. }) =
        message.envelope.payload
    else {
        panic!("expected screenshot request");
    };
    client
        .respond_screenshot(
            &producer,
            ScreenshotResponse {
                request_id: request_id.clone(),
                status: ControlStatus::Ok,
                screenshot: Some(ScreenshotPayload {
                    width: 2,
                    height: 1,
                    media_type: ScreenshotMediaType::Png,
                    data_base64: "iVBORw0KGgo=".to_string(),
                }),
                error: None,
            },
        )
        .await?;
    let received: CodeBridgeScreenshotResponse =
        response_for(&mut app_server, app_request_id).await?;

    assert_eq!(
        received,
        CodeBridgeScreenshotResponse {
            request_id,
            status: CodeBridgeControlStatus::Ok,
            screenshot: Some(CodeBridgeScreenshotPayload {
                width: 2,
                height: 1,
                media_type: CodeBridgeScreenshotMediaType::Png,
                data_base64: "iVBORw0KGgo=".to_string(),
            }),
            error: None,
        }
    );

    service.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn code_bridge_javascript_round_trips_through_jsonrpc() -> Result<()> {
    let codex_home = TempDir::new()?;
    let service =
        codex_code_bridge_service::start(BridgeServiceConfig::new(codex_home.path().to_path_buf()))
            .await?;
    let mut app_server = initialized_app_server(&codex_home).await?;
    let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())?;
    let (producer, mut producer_events) = producer_session(
        &client,
        CapabilitySet {
            provide_control: true,
            provide_javascript_execution: true,
            ..CapabilitySet::default()
        },
    )
    .await?;

    let app_request_id = app_server
        .send_raw_request(
            "codeBridge/javascript",
            Some(json!({
                "targetClientId": "producer-1",
                "code": "window.location.href",
                "timeoutMs": 1_000,
            })),
        )
        .await?;
    let message = next_bridge_message(&mut producer_events, "control request").await;
    let BridgePayload::ControlRequest(ControlRequestMessage {
        request_id,
        command,
        ..
    }) = message.envelope.payload
    else {
        panic!("expected control request");
    };
    assert_eq!(
        command,
        ControlCommand::ExecuteJavascript {
            code: "window.location.href".to_string(),
        }
    );
    client
        .respond_control_with_result(
            &producer,
            ControlResponse {
                request_id: request_id.clone(),
                status: ControlStatus::Ok,
                summary: "https://example.test/".to_string(),
                error: None,
            },
            json!("https://example.test/"),
        )
        .await?;
    let received: CodeBridgeJavascriptResponse =
        response_for(&mut app_server, app_request_id).await?;

    assert_eq!(
        received,
        CodeBridgeJavascriptResponse {
            request_id,
            status: CodeBridgeControlStatus::Ok,
            summary: "https://example.test/".to_string(),
            result: Some(json!("https://example.test/")),
            error: None,
        }
    );

    service.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn code_bridge_javascript_timeout_returns_typed_response() -> Result<()> {
    let codex_home = TempDir::new()?;
    let service =
        codex_code_bridge_service::start(BridgeServiceConfig::new(codex_home.path().to_path_buf()))
            .await?;
    let mut app_server = initialized_app_server(&codex_home).await?;
    let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())?;
    let (_producer, _producer_events) = producer_session(
        &client,
        CapabilitySet {
            provide_control: true,
            provide_javascript_execution: true,
            ..CapabilitySet::default()
        },
    )
    .await?;

    let received: CodeBridgeJavascriptResponse = jsonrpc_response(
        &mut app_server,
        "codeBridge/javascript",
        json!({
            "targetClientId": "producer-1",
            "code": "window.location.href",
            "timeoutMs": 1,
        }),
    )
    .await?;

    assert_eq!(received.status, CodeBridgeControlStatus::TimedOut);
    assert_eq!(received.summary, "");
    assert_eq!(received.result, None);
    let error = received.error.expect("timeout error");
    assert_eq!(error.code, CodeBridgeErrorCode::Timeout);

    service.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn code_bridge_screenshot_timeout_returns_typed_response() -> Result<()> {
    let codex_home = TempDir::new()?;
    let service =
        codex_code_bridge_service::start(BridgeServiceConfig::new(codex_home.path().to_path_buf()))
            .await?;
    let mut app_server = initialized_app_server(&codex_home).await?;
    let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())?;
    let (_producer, _producer_events) = producer_session(
        &client,
        CapabilitySet {
            provide_screenshot: true,
            ..CapabilitySet::default()
        },
    )
    .await?;

    let received: CodeBridgeScreenshotResponse = jsonrpc_response(
        &mut app_server,
        "codeBridge/screenshot",
        json!({
            "targetClientId": "producer-1",
            "timeoutMs": 1,
        }),
    )
    .await?;

    assert_eq!(received.status, CodeBridgeControlStatus::TimedOut);
    assert_eq!(received.screenshot, None);
    let error = received.error.expect("timeout error");
    assert_eq!(error.code, CodeBridgeErrorCode::Timeout);

    service.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn code_bridge_screenshot_rejects_invalid_timeout() -> Result<()> {
    let codex_home = TempDir::new()?;
    let service =
        codex_code_bridge_service::start(BridgeServiceConfig::new(codex_home.path().to_path_buf()))
            .await?;
    let mut app_server = initialized_app_server(&codex_home).await?;

    let request_id = app_server
        .send_raw_request(
            "codeBridge/screenshot",
            Some(json!({
                "targetClientId": "producer-1",
                "timeoutMs": 0,
            })),
        )
        .await?;
    let error: JSONRPCError = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;

    assert_eq!(error.error.code, -32602);
    assert!(error.error.message.contains("timeoutMs must be between"));

    service.shutdown().await;
    Ok(())
}

async fn client_status(
    descriptor_path: &std::path::Path,
) -> Result<codex_code_bridge_protocol::BridgeServiceStatus> {
    Ok(CodeBridgeClient::from_descriptor_path(descriptor_path)?
        .status()
        .await?)
}

async fn initialized_app_server(codex_home: &TempDir) -> Result<TestAppServer> {
    let mut app_server = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, app_server.initialize()).await??;
    Ok(app_server)
}

async fn jsonrpc_response<T: DeserializeOwned>(
    app_server: &mut TestAppServer,
    method: &str,
    params: serde_json::Value,
) -> Result<T> {
    let request_id = app_server.send_raw_request(method, Some(params)).await?;
    response_for(app_server, request_id).await
}

async fn response_for<T: DeserializeOwned>(
    app_server: &mut TestAppServer,
    request_id: i64,
) -> Result<T> {
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(response)
}

async fn producer_session(
    client: &CodeBridgeClient,
    capabilities: CapabilitySet,
) -> Result<(CodeBridgeSession, CodeBridgeEventStream)> {
    let producer = client
        .hello(
            "producer-1",
            ClientRole::Producer,
            capabilities,
            test_metadata("producer"),
        )
        .await?;
    let events = client.events(&producer, 0).await?;
    Ok((producer, events))
}

fn test_metadata(label: &str) -> ClientMetadata {
    ClientMetadata {
        source_kind: SourceKind::TestFixture,
        label: Some(label.to_string()),
        provenance: None,
    }
}

async fn next_bridge_message(
    stream: &mut CodeBridgeEventStream,
    context: &str,
) -> BridgeSseMessage {
    timeout(DEFAULT_TIMEOUT, stream.next_message())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {context}"))
        .unwrap_or_else(|err| panic!("failed to read {context}: {err}"))
}
