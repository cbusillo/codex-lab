use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use codex_app_server_protocol::CodeBridgeAvailability;
use codex_app_server_protocol::CodeBridgeStatusReadResponse;
use codex_app_server_protocol::CodeBridgeUnavailableReason;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_code_bridge_protocol::PROTOCOL_VERSION;
use codex_code_bridge_service::BridgeServiceConfig;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn code_bridge_status_read_reports_missing_descriptor() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut app_server = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, app_server.initialize()).await??;

    let request_id = app_server.send_code_bridge_status_read_request().await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let received: CodeBridgeStatusReadResponse = to_response(response)?;

    assert_eq!(received.status, CodeBridgeAvailability::Unavailable);
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
    let mut app_server = TestAppServer::new(codex_home.path()).await?;
    timeout(DEFAULT_TIMEOUT, app_server.initialize()).await??;

    let request_id = app_server.send_code_bridge_status_read_request().await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let received: CodeBridgeStatusReadResponse = to_response(response)?;

    assert_eq!(received.status, CodeBridgeAvailability::Available);
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
    let response: JSONRPCResponse = timeout(
        DEFAULT_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let received: CodeBridgeStatusReadResponse = to_response(response)?;

    assert_eq!(received.status, CodeBridgeAvailability::Available);
    assert_eq!(received.unavailable_reason, None);
    assert_eq!(received.service, None);
    accept_task.await?;
    Ok(())
}
