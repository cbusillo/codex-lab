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
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

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
