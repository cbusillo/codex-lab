use codex_app_server_protocol::CodeBridgeAvailability;
use codex_app_server_protocol::CodeBridgeServiceStatus as ApiCodeBridgeServiceStatus;
use codex_app_server_protocol::CodeBridgeStatusReadResponse;
use codex_app_server_protocol::CodeBridgeUnavailableReason;
use codex_code_bridge_client::CodeBridgeClient;
use codex_code_bridge_client::CodeBridgeClientError;
use codex_code_bridge_protocol::BridgeServiceStatus;
use codex_code_bridge_protocol::DESCRIPTOR_RELATIVE_PATH;
use futures::SinkExt;
use futures::StreamExt;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use tokio::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Host;
use url::Url;

const STATUS_READ_TIMEOUT: Duration = Duration::from_secs(2);
const WORKSPACE_META_FILE: &str = "code-bridge.json";

#[derive(Clone)]
pub(crate) struct CodeBridgeRequestProcessor {
    descriptor_path: PathBuf,
    workspace_cwd: PathBuf,
}

impl CodeBridgeRequestProcessor {
    pub(crate) fn new(codex_home: PathBuf, workspace_cwd: PathBuf) -> Self {
        Self {
            descriptor_path: codex_home.join(DESCRIPTOR_RELATIVE_PATH),
            workspace_cwd,
        }
    }

    pub(crate) async fn status_read(&self) -> CodeBridgeStatusReadResponse {
        let home_response = self.home_descriptor_status_read().await;
        if home_response.status == CodeBridgeAvailability::Available {
            return home_response;
        }
        self.workspace_metadata_status_read()
            .await
            .unwrap_or(home_response)
    }

    pub(super) fn descriptor_path(&self) -> &Path {
        &self.descriptor_path
    }

    async fn home_descriptor_status_read(&self) -> CodeBridgeStatusReadResponse {
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

    async fn workspace_metadata_status_read(&self) -> Option<CodeBridgeStatusReadResponse> {
        let meta_path = find_workspace_bridge_meta(&self.workspace_cwd)?;
        let meta = match read_workspace_bridge_meta(&meta_path) {
            Ok(meta) => meta,
            Err(_) => return Some(unavailable(CodeBridgeUnavailableReason::DescriptorInvalid)),
        };
        Some(
            match timeout(STATUS_READ_TIMEOUT, probe_workspace_bridge(&meta)).await {
                Ok(Ok(())) => available_workspace_metadata_bridge(),
                Ok(Err(reason)) => unavailable(reason),
                Err(_) => unavailable(CodeBridgeUnavailableReason::ServiceUnreachable),
            },
        )
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

fn available_workspace_metadata_bridge() -> CodeBridgeStatusReadResponse {
    CodeBridgeStatusReadResponse {
        status: CodeBridgeAvailability::Available,
        service: None,
        unavailable_reason: None,
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceBridgeMeta {
    url: String,
    secret: String,
}

fn find_workspace_bridge_meta(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let candidate = dir.join(".code").join(WORKSPACE_META_FILE);
        if candidate.exists() {
            return Some(candidate);
        }
        current = dir.parent();
    }
    None
}

fn read_workspace_bridge_meta(path: &Path) -> io::Result<WorkspaceBridgeMeta> {
    let raw = std::fs::read(path)?;
    serde_json::from_slice(&raw).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

async fn probe_workspace_bridge(
    meta: &WorkspaceBridgeMeta,
) -> Result<(), CodeBridgeUnavailableReason> {
    let url = Url::parse(&meta.url).map_err(|_| CodeBridgeUnavailableReason::DescriptorInvalid)?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(CodeBridgeUnavailableReason::UnsupportedEndpoint);
    }
    if !is_loopback_bridge_host(&url) {
        return Err(CodeBridgeUnavailableReason::UnsupportedEndpoint);
    }
    let (mut ws, _) = connect_async(meta.url.as_str())
        .await
        .map_err(|_| CodeBridgeUnavailableReason::ServiceUnreachable)?;
    let auth = serde_json::json!({
        "type": "auth",
        "role": "consumer",
        "secret": meta.secret,
        "clientId": "codex-lab-app-server-status",
    });
    ws.send(Message::Text(auth.to_string().into()))
        .await
        .map_err(|_| CodeBridgeUnavailableReason::ServiceUnreachable)?;

    while let Some(message) = ws.next().await {
        let message = message.map_err(|_| CodeBridgeUnavailableReason::ServiceUnreachable)?;
        let Message::Text(text) = message else {
            continue;
        };
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| CodeBridgeUnavailableReason::StatusInvalid)?;
        match value.get("type").and_then(|value| value.as_str()) {
            Some("auth_success") => {
                let _ = ws.send(Message::Close(None)).await;
                return Ok(());
            }
            Some("auth_error") | Some("error") => {
                return Err(CodeBridgeUnavailableReason::DescriptorInvalid);
            }
            _ => {}
        }
    }
    Err(CodeBridgeUnavailableReason::ServiceUnreachable)
}

fn is_loopback_bridge_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
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
    use futures::SinkExt;
    use futures::StreamExt;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn status_read_reports_missing_descriptor_as_unavailable() {
        let codex_home = TempDir::new().expect("temp home");
        let workspace = TempDir::new().expect("workspace");
        let processor = CodeBridgeRequestProcessor::new(
            codex_home.path().to_path_buf(),
            workspace.path().to_path_buf(),
        );

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
        let workspace = TempDir::new().expect("workspace");
        let service = codex_code_bridge_service::start(BridgeServiceConfig::new(
            codex_home.path().to_path_buf(),
        ))
        .await
        .expect("start service");
        let processor = CodeBridgeRequestProcessor::new(
            codex_home.path().to_path_buf(),
            workspace.path().to_path_buf(),
        );

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
        let workspace = TempDir::new().expect("workspace");
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
        let processor = CodeBridgeRequestProcessor::new(
            codex_home.path().to_path_buf(),
            workspace.path().to_path_buf(),
        );

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

    #[tokio::test]
    async fn status_read_falls_back_to_workspace_bridge_metadata() {
        let codex_home = TempDir::new().expect("temp home");
        let workspace = TempDir::new().expect("workspace");
        let nested = workspace.path().join("packages/app");
        std::fs::create_dir_all(&nested).expect("nested workspace");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind workspace bridge");
        let local_addr = listener.local_addr().expect("local addr");
        let code_dir = workspace.path().join(".code");
        std::fs::create_dir_all(&code_dir).expect("code dir");
        std::fs::write(
            code_dir.join(WORKSPACE_META_FILE),
            serde_json::json!({
                "url": format!("ws://{local_addr}"),
                "secret": "workspace-secret",
                "port": local_addr.port(),
                "workspacePath": workspace.path(),
            })
            .to_string(),
        )
        .expect("write bridge metadata");
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
        let processor = CodeBridgeRequestProcessor::new(codex_home.path().to_path_buf(), nested);

        let response = processor.status_read().await;

        assert_eq!(response.status, CodeBridgeAvailability::Available);
        assert_eq!(response.unavailable_reason, None);
        assert_eq!(response.service, None);
        accept_task.await.expect("accept task");
    }

    #[tokio::test]
    async fn status_read_rejects_non_loopback_workspace_bridge_metadata() {
        let codex_home = TempDir::new().expect("temp home");
        let workspace = TempDir::new().expect("workspace");
        let code_dir = workspace.path().join(".code");
        std::fs::create_dir_all(&code_dir).expect("code dir");
        std::fs::write(
            code_dir.join(WORKSPACE_META_FILE),
            serde_json::json!({
                "url": "ws://example.com:12345",
                "secret": "workspace-secret",
            })
            .to_string(),
        )
        .expect("write bridge metadata");
        let processor = CodeBridgeRequestProcessor::new(
            codex_home.path().to_path_buf(),
            workspace.path().to_path_buf(),
        );

        let response = processor.status_read().await;

        assert_eq!(response.status, CodeBridgeAvailability::Unavailable);
        assert_eq!(
            response.unavailable_reason,
            Some(CodeBridgeUnavailableReason::UnsupportedEndpoint)
        );
        assert_eq!(response.service, None);
    }
}
