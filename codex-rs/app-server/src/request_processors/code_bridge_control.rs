use crate::error_code::internal_error;
use crate::error_code::invalid_params;
use crate::request_processors::code_bridge_processor::CodeBridgeRequestProcessor;
use codex_app_server_protocol::CodeBridgeConsoleLevel;
use codex_app_server_protocol::CodeBridgeControlStatus;
use codex_app_server_protocol::CodeBridgeError;
use codex_app_server_protocol::CodeBridgeErrorCode;
use codex_app_server_protocol::CodeBridgeEventKind;
use codex_app_server_protocol::CodeBridgeJavascriptParams;
use codex_app_server_protocol::CodeBridgeJavascriptResponse;
use codex_app_server_protocol::CodeBridgeRequestStatus;
use codex_app_server_protocol::CodeBridgeScreenshotMediaType;
use codex_app_server_protocol::CodeBridgeScreenshotParams;
use codex_app_server_protocol::CodeBridgeScreenshotPayload;
use codex_app_server_protocol::CodeBridgeScreenshotResponse;
use codex_app_server_protocol::CodeBridgeSubscribeParams;
use codex_app_server_protocol::CodeBridgeSubscribeResponse;
use codex_app_server_protocol::CodeBridgeSubscriptionFilter;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_code_bridge_client::CodeBridgeClient;
use codex_code_bridge_client::CodeBridgeClientError;
use codex_code_bridge_client::CodeBridgeEventStream;
use codex_code_bridge_client::CodeBridgeSession;
use codex_code_bridge_client::ControlRequest;
use codex_code_bridge_client::ScreenshotRequest;
use codex_code_bridge_protocol::BridgePayload;
use codex_code_bridge_protocol::CapabilitySet;
use codex_code_bridge_protocol::ClientMetadata;
use codex_code_bridge_protocol::ClientRole;
use codex_code_bridge_protocol::ControlCommand;
use codex_code_bridge_protocol::ControlResponseMessage;
use codex_code_bridge_protocol::ControlStatus;
use codex_code_bridge_protocol::ErrorCode;
use codex_code_bridge_protocol::ErrorMessage;
use codex_code_bridge_protocol::EventKind;
use codex_code_bridge_protocol::MAX_CONTROL_TIMEOUT_MS;
use codex_code_bridge_protocol::MAX_EVENT_TEXT_BYTES;
use codex_code_bridge_protocol::ScreenshotMediaType;
use codex_code_bridge_protocol::ScreenshotResponseMessage;
use codex_code_bridge_protocol::SourceKind;
use codex_code_bridge_protocol::SubscriptionFilter;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::time::Duration;
use tokio::time::timeout;

const CONTROL_RESPONSE_GRACE_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_CONTROL_TIMEOUT_MS: u64 = MAX_CONTROL_TIMEOUT_MS;
const MAX_CODE_BRIDGE_FILTER_ITEMS: usize = 64;
const MAX_CODE_BRIDGE_ID_BYTES: usize = 256;
static NEXT_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

impl CodeBridgeRequestProcessor {
    pub(crate) async fn subscribe(
        &self,
        params: CodeBridgeSubscribeParams,
    ) -> Result<CodeBridgeSubscribeResponse, JSONRPCErrorError> {
        validate_filter(&params.filter)?;
        let client = self.client()?;
        let session = subscriber_session(
            &client,
            "app-server-subscribe",
            CapabilitySet {
                subscribe_events: true,
                ..CapabilitySet::default()
            },
        )
        .await?;
        expect_ack(
            client
                .subscribe(&session, map_filter(params.filter))
                .await
                .map_err(map_client_error)?,
        )?;
        Ok(CodeBridgeSubscribeResponse {
            status: CodeBridgeRequestStatus::Accepted,
        })
    }

    pub(crate) async fn screenshot(
        &self,
        params: CodeBridgeScreenshotParams,
    ) -> Result<CodeBridgeScreenshotResponse, JSONRPCErrorError> {
        validate_id("targetClientId", &params.target_client_id)?;
        let timeout_ms = validate_timeout(params.timeout_ms)?;
        let client = self.client()?;
        let session = subscriber_session(
            &client,
            "app-server-screenshot",
            CapabilitySet {
                subscribe_events: true,
                request_screenshot: true,
                ..CapabilitySet::default()
            },
        )
        .await?;
        let mut events = client.events(&session, 0).await.map_err(map_client_error)?;
        let request_id = request_id("screenshot");
        expect_ack(
            client
                .request_screenshot(
                    &session,
                    ScreenshotRequest {
                        request_id: request_id.clone(),
                        target_client_id: params.target_client_id,
                        timeout_ms,
                    },
                )
                .await
                .map_err(map_client_error)?,
        )?;
        let response = wait_for_screenshot_response(&mut events, &request_id, timeout_ms).await?;
        Ok(map_screenshot_response(response))
    }

    pub(crate) async fn javascript(
        &self,
        params: CodeBridgeJavascriptParams,
    ) -> Result<CodeBridgeJavascriptResponse, JSONRPCErrorError> {
        validate_id("targetClientId", &params.target_client_id)?;
        validate_text_bytes("code", &params.code, MAX_EVENT_TEXT_BYTES)?;
        let timeout_ms = validate_timeout(params.timeout_ms)?;
        let client = self.client()?;
        let session = subscriber_session(
            &client,
            "app-server-javascript",
            CapabilitySet {
                subscribe_events: true,
                request_control: true,
                ..CapabilitySet::default()
            },
        )
        .await?;
        let mut events = client.events(&session, 0).await.map_err(map_client_error)?;
        let request_id = request_id("javascript");
        expect_ack(
            client
                .request_control(
                    &session,
                    ControlRequest {
                        request_id: request_id.clone(),
                        target_client_id: params.target_client_id,
                        command: ControlCommand::ExecuteJavascript { code: params.code },
                        timeout_ms,
                    },
                )
                .await
                .map_err(map_client_error)?,
        )?;
        let response = wait_for_control_response(&mut events, &request_id, timeout_ms).await?;
        Ok(map_control_response(response))
    }

    fn client(&self) -> Result<CodeBridgeClient, JSONRPCErrorError> {
        CodeBridgeClient::from_descriptor_path(self.descriptor_path()).map_err(map_client_error)
    }
}

async fn subscriber_session(
    client: &CodeBridgeClient,
    client_id_prefix: &str,
    capabilities: CapabilitySet,
) -> Result<CodeBridgeSession, JSONRPCErrorError> {
    client
        .hello(
            request_id(client_id_prefix),
            ClientRole::Subscriber,
            capabilities,
            ClientMetadata {
                source_kind: SourceKind::Cli,
                label: Some("app-server".to_string()),
                provenance: None,
            },
        )
        .await
        .map_err(map_client_error)
}

async fn wait_for_screenshot_response(
    events: &mut CodeBridgeEventStream,
    request_id: &str,
    timeout_ms: u64,
) -> Result<ScreenshotResponseMessage, JSONRPCErrorError> {
    timeout(response_wait_timeout(timeout_ms), async {
        loop {
            let message = events.next_message().await.map_err(map_client_error)?;
            if let BridgePayload::ScreenshotResponse(response) = message.envelope.payload
                && response.request_id == request_id
            {
                return Ok(response);
            }
        }
    })
    .await
    .unwrap_or_else(|_| Ok(screenshot_timeout_response(request_id)))
}

async fn wait_for_control_response(
    events: &mut CodeBridgeEventStream,
    request_id: &str,
    timeout_ms: u64,
) -> Result<ControlResponseMessage, JSONRPCErrorError> {
    timeout(response_wait_timeout(timeout_ms), async {
        loop {
            let message = events.next_message().await.map_err(map_client_error)?;
            if let BridgePayload::ControlResponse(response) = message.envelope.payload
                && response.request_id == request_id
            {
                return Ok(response);
            }
        }
    })
    .await
    .unwrap_or_else(|_| Ok(control_timeout_response(request_id)))
}

fn response_wait_timeout(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms) + CONTROL_RESPONSE_GRACE_TIMEOUT
}

fn screenshot_timeout_response(request_id: &str) -> ScreenshotResponseMessage {
    ScreenshotResponseMessage {
        request_id: request_id.to_string(),
        responding_client_id: String::new(),
        status: ControlStatus::TimedOut,
        screenshot: None,
        error: Some(timeout_error(request_id)),
    }
}

fn control_timeout_response(request_id: &str) -> ControlResponseMessage {
    ControlResponseMessage {
        request_id: request_id.to_string(),
        responding_client_id: String::new(),
        status: ControlStatus::TimedOut,
        summary: String::new(),
        result: None,
        error: Some(timeout_error(request_id)),
    }
}

fn timeout_error(request_id: &str) -> ErrorMessage {
    ErrorMessage {
        code: ErrorCode::Timeout,
        message: format!("Code Bridge request {request_id} timed out"),
    }
}

fn request_id(prefix: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = NEXT_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{timestamp}-{sequence}")
}

fn expect_ack(payload: BridgePayload) -> Result<(), JSONRPCErrorError> {
    match payload {
        BridgePayload::Ack(_) => Ok(()),
        BridgePayload::Error(error) => Err(map_bridge_error(error)),
        other => Err(internal_error(format!(
            "unexpected Code Bridge response payload {other:?}"
        ))),
    }
}

fn map_bridge_error(error: ErrorMessage) -> JSONRPCErrorError {
    match error.code {
        ErrorCode::InvalidPayload | ErrorCode::PayloadTooLarge | ErrorCode::Timeout => {
            invalid_params(error.message)
        }
        ErrorCode::AuthRequired
        | ErrorCode::AuthRejected
        | ErrorCode::CapabilityDenied
        | ErrorCode::UnsupportedProtocolVersion => internal_error(error.message),
    }
}

fn map_client_error(error: CodeBridgeClientError) -> JSONRPCErrorError {
    internal_error(format!("Code Bridge request failed: {error}"))
}

fn validate_filter(filter: &CodeBridgeSubscriptionFilter) -> Result<(), JSONRPCErrorError> {
    if filter.levels.len() > MAX_CODE_BRIDGE_FILTER_ITEMS {
        return Err(invalid_params(format!(
            "levels must contain at most {MAX_CODE_BRIDGE_FILTER_ITEMS} items"
        )));
    }
    if filter.event_kinds.len() > MAX_CODE_BRIDGE_FILTER_ITEMS {
        return Err(invalid_params(format!(
            "eventKinds must contain at most {MAX_CODE_BRIDGE_FILTER_ITEMS} items"
        )));
    }
    if filter.client_ids.len() > MAX_CODE_BRIDGE_FILTER_ITEMS {
        return Err(invalid_params(format!(
            "clientIds must contain at most {MAX_CODE_BRIDGE_FILTER_ITEMS} items"
        )));
    }
    for client_id in &filter.client_ids {
        validate_id("clientIds", client_id)?;
    }
    Ok(())
}

fn validate_id(field_name: &str, value: &str) -> Result<(), JSONRPCErrorError> {
    validate_text_bytes(field_name, value, MAX_CODE_BRIDGE_ID_BYTES)
}

fn validate_text_bytes(
    field_name: &str,
    value: &str,
    max_bytes: usize,
) -> Result<(), JSONRPCErrorError> {
    if value.is_empty() {
        return Err(invalid_params(format!("{field_name} must not be empty")));
    }
    if value.len() > max_bytes {
        return Err(invalid_params(format!(
            "{field_name} must be at most {max_bytes} bytes"
        )));
    }
    Ok(())
}

fn validate_timeout(timeout_ms: Option<u64>) -> Result<u64, JSONRPCErrorError> {
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_CONTROL_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > MAX_CONTROL_TIMEOUT_MS {
        return Err(invalid_params(format!(
            "timeoutMs must be between 1 and {MAX_CONTROL_TIMEOUT_MS}"
        )));
    }
    Ok(timeout_ms)
}

fn map_filter(filter: CodeBridgeSubscriptionFilter) -> SubscriptionFilter {
    SubscriptionFilter {
        levels: filter.levels.into_iter().map(map_console_level).collect(),
        event_kinds: filter.event_kinds.into_iter().map(map_event_kind).collect(),
        client_ids: filter.client_ids,
    }
}

fn map_console_level(level: CodeBridgeConsoleLevel) -> codex_code_bridge_protocol::ConsoleLevel {
    match level {
        CodeBridgeConsoleLevel::Trace => codex_code_bridge_protocol::ConsoleLevel::Trace,
        CodeBridgeConsoleLevel::Info => codex_code_bridge_protocol::ConsoleLevel::Info,
        CodeBridgeConsoleLevel::Warn => codex_code_bridge_protocol::ConsoleLevel::Warn,
        CodeBridgeConsoleLevel::Error => codex_code_bridge_protocol::ConsoleLevel::Error,
    }
}

fn map_event_kind(kind: CodeBridgeEventKind) -> EventKind {
    match kind {
        CodeBridgeEventKind::Console => EventKind::Console,
        CodeBridgeEventKind::Error => EventKind::Error,
        CodeBridgeEventKind::Pageview => EventKind::Pageview,
        CodeBridgeEventKind::Screenshot => EventKind::Screenshot,
        CodeBridgeEventKind::ControlResult => EventKind::ControlResult,
    }
}

fn map_screenshot_response(response: ScreenshotResponseMessage) -> CodeBridgeScreenshotResponse {
    CodeBridgeScreenshotResponse {
        request_id: response.request_id,
        status: map_control_status(response.status),
        screenshot: response.screenshot.map(map_screenshot_payload),
        error: response.error.map(map_api_error),
    }
}

fn map_control_response(response: ControlResponseMessage) -> CodeBridgeJavascriptResponse {
    CodeBridgeJavascriptResponse {
        request_id: response.request_id,
        status: map_control_status(response.status),
        summary: response.summary,
        result: response.result,
        error: response.error.map(map_api_error),
    }
}

fn map_screenshot_payload(
    payload: codex_code_bridge_protocol::ScreenshotPayload,
) -> CodeBridgeScreenshotPayload {
    CodeBridgeScreenshotPayload {
        width: payload.width,
        height: payload.height,
        media_type: map_screenshot_media_type(payload.media_type),
        data_base64: payload.data_base64,
    }
}

fn map_screenshot_media_type(media_type: ScreenshotMediaType) -> CodeBridgeScreenshotMediaType {
    match media_type {
        ScreenshotMediaType::Png => CodeBridgeScreenshotMediaType::Png,
        ScreenshotMediaType::Jpeg => CodeBridgeScreenshotMediaType::Jpeg,
    }
}

fn map_control_status(status: ControlStatus) -> CodeBridgeControlStatus {
    match status {
        ControlStatus::Ok => CodeBridgeControlStatus::Ok,
        ControlStatus::Failed => CodeBridgeControlStatus::Failed,
        ControlStatus::TimedOut => CodeBridgeControlStatus::TimedOut,
        ControlStatus::Denied => CodeBridgeControlStatus::Denied,
    }
}

fn map_api_error(error: ErrorMessage) -> CodeBridgeError {
    CodeBridgeError {
        code: map_error_code(error.code),
        message: error.message,
    }
}

fn map_error_code(code: ErrorCode) -> CodeBridgeErrorCode {
    match code {
        ErrorCode::AuthRequired => CodeBridgeErrorCode::AuthRequired,
        ErrorCode::AuthRejected => CodeBridgeErrorCode::AuthRejected,
        ErrorCode::CapabilityDenied => CodeBridgeErrorCode::CapabilityDenied,
        ErrorCode::InvalidPayload => CodeBridgeErrorCode::InvalidPayload,
        ErrorCode::PayloadTooLarge => CodeBridgeErrorCode::PayloadTooLarge,
        ErrorCode::Timeout => CodeBridgeErrorCode::Timeout,
        ErrorCode::UnsupportedProtocolVersion => CodeBridgeErrorCode::UnsupportedProtocolVersion,
    }
}
