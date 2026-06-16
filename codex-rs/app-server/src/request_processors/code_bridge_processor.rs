use codex_app_server_protocol::CodeBridgeAvailability;
use codex_app_server_protocol::CodeBridgeServiceStatus as ApiCodeBridgeServiceStatus;
use codex_app_server_protocol::CodeBridgeStatusReadResponse;
use codex_app_server_protocol::CodeBridgeUnavailableReason;
use codex_code_bridge_client::CodeBridgeClient;
use codex_code_bridge_client::CodeBridgeClientError;
use codex_code_bridge_protocol::BridgeServiceStatus;
use codex_code_bridge_protocol::DESCRIPTOR_RELATIVE_PATH;
use std::io;
use std::path::PathBuf;
use tokio::time::Duration;
use tokio::time::timeout;

const STATUS_READ_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct CodeBridgeRequestProcessor {
    descriptor_path: PathBuf,
}

impl CodeBridgeRequestProcessor {
    pub(crate) fn new(codex_home: PathBuf) -> Self {
        Self {
            descriptor_path: codex_home.join(DESCRIPTOR_RELATIVE_PATH),
        }
    }

    pub(crate) async fn status_read(&self) -> CodeBridgeStatusReadResponse {
        let client = match CodeBridgeClient::from_descriptor_path(&self.descriptor_path) {
            Ok(client) => client,
            Err(error) => return unavailable(map_descriptor_error(&error)),
        };
        match timeout(STATUS_READ_TIMEOUT, client.status()).await {
            Ok(Ok(status)) => available(status),
            Ok(Err(error)) => unavailable(map_status_error(&error)),
            Err(_) => unavailable(CodeBridgeUnavailableReason::ServiceUnreachable),
        }
    }
}

fn available(status: BridgeServiceStatus) -> CodeBridgeStatusReadResponse {
    CodeBridgeStatusReadResponse {
        status: CodeBridgeAvailability::Available,
        service: Some(ApiCodeBridgeServiceStatus {
            protocol_version: status.protocol_version,
            connected_producer_count: status.connected_producer_count,
            connected_subscriber_count: status.connected_subscriber_count,
            uptime_ms: status.uptime_ms,
            last_event_time_unix_ms: status.last_event_time_unix_ms,
        }),
        unavailable_reason: None,
    }
}

fn unavailable(reason: CodeBridgeUnavailableReason) -> CodeBridgeStatusReadResponse {
    CodeBridgeStatusReadResponse {
        status: CodeBridgeAvailability::Unavailable,
        service: None,
        unavailable_reason: Some(reason),
    }
}

fn map_descriptor_error(error: &CodeBridgeClientError) -> CodeBridgeUnavailableReason {
    match error {
        CodeBridgeClientError::ReadDescriptor { source, .. }
            if source.kind() == io::ErrorKind::NotFound =>
        {
            CodeBridgeUnavailableReason::DescriptorMissing
        }
        CodeBridgeClientError::ReadDescriptor { .. }
        | CodeBridgeClientError::ParseDescriptor { .. }
        | CodeBridgeClientError::InvalidDescriptor(_)
        | CodeBridgeClientError::InvalidEndpointUrl(_) => {
            CodeBridgeUnavailableReason::DescriptorInvalid
        }
        CodeBridgeClientError::UnsupportedEndpoint => {
            CodeBridgeUnavailableReason::UnsupportedEndpoint
        }
        _ => CodeBridgeUnavailableReason::DescriptorInvalid,
    }
}

fn map_status_error(error: &CodeBridgeClientError) -> CodeBridgeUnavailableReason {
    match error {
        CodeBridgeClientError::Http(error) if error.is_decode() => {
            CodeBridgeUnavailableReason::StatusInvalid
        }
        CodeBridgeClientError::HttpStatus(status) if matches!(status.as_u16(), 401 | 403) => {
            CodeBridgeUnavailableReason::DescriptorInvalid
        }
        _ => CodeBridgeUnavailableReason::ServiceUnreachable,
    }
}

#[cfg(test)]
mod code_bridge_processor_tests {
    use super::*;
    use codex_code_bridge_protocol::BridgeDescriptor;
    use codex_code_bridge_protocol::BridgeEndpoint;
    use codex_code_bridge_protocol::PROTOCOL_VERSION;
    use codex_code_bridge_protocol::validate_descriptor;
    use codex_code_bridge_service::BridgeServiceConfig;
    use tempfile::TempDir;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn status_read_reports_missing_descriptor_as_unavailable() {
        let codex_home = TempDir::new().expect("temp home");
        let processor = CodeBridgeRequestProcessor::new(codex_home.path().to_path_buf());

        let response = processor.status_read().await;

        assert_eq!(response.status, CodeBridgeAvailability::Unavailable);
        assert_eq!(
            response.unavailable_reason,
            Some(CodeBridgeUnavailableReason::DescriptorMissing)
        );
        assert_eq!(response.service, None);
    }

    #[tokio::test]
    async fn status_read_reports_running_service_as_available() {
        let codex_home = TempDir::new().expect("temp home");
        let service = codex_code_bridge_service::start(BridgeServiceConfig::new(
            codex_home.path().to_path_buf(),
        ))
        .await
        .expect("start service");
        let processor = CodeBridgeRequestProcessor::new(codex_home.path().to_path_buf());

        let response = processor.status_read().await;

        assert_eq!(response.status, CodeBridgeAvailability::Available);
        assert_eq!(response.unavailable_reason, None);
        let service_status = response.service.expect("service status");
        assert_eq!(
            service_status.protocol_version,
            codex_code_bridge_protocol::PROTOCOL_VERSION
        );
        assert_eq!(service_status.connected_producer_count, 0);
        assert_eq!(service_status.connected_subscriber_count, 0);

        service.shutdown().await;
    }

    #[tokio::test]
    async fn status_read_times_out_hung_descriptor_endpoint() {
        let codex_home = TempDir::new().expect("temp home");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let local_addr = listener.local_addr().expect("local addr");
        let descriptor = BridgeDescriptor {
            protocol_version: PROTOCOL_VERSION.to_string(),
            endpoint: BridgeEndpoint::LoopbackHttp {
                url: format!("http://{local_addr}"),
            },
            auth_secret: "test-secret".to_string(),
            pid: None,
        };
        validate_descriptor(&descriptor).expect("valid descriptor");
        let descriptor_path = codex_home.path().join(DESCRIPTOR_RELATIVE_PATH);
        std::fs::create_dir_all(descriptor_path.parent().expect("descriptor parent"))
            .expect("create descriptor parent");
        std::fs::write(
            &descriptor_path,
            serde_json::to_vec(&descriptor).expect("serialize descriptor"),
        )
        .expect("write descriptor");
        let accept_task = tokio::spawn(async move {
            let (_socket, _addr) = listener.accept().await.expect("accept connection");
            tokio::time::sleep(STATUS_READ_TIMEOUT * 2).await;
        });
        let processor = CodeBridgeRequestProcessor::new(codex_home.path().to_path_buf());

        let response = processor.status_read().await;

        assert_eq!(response.status, CodeBridgeAvailability::Unavailable);
        assert_eq!(
            response.unavailable_reason,
            Some(CodeBridgeUnavailableReason::ServiceUnreachable)
        );
        assert_eq!(response.service, None);
        accept_task.abort();
        let _ = accept_task.await;
    }
}
