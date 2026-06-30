use codex_code_bridge_protocol::AuthProof;
use codex_code_bridge_protocol::BridgeDescriptor;
use codex_code_bridge_protocol::BridgeEndpoint;
use codex_code_bridge_protocol::BridgeEnvelope;
use codex_code_bridge_protocol::BridgeEvent;
use codex_code_bridge_protocol::BridgeMessageResponse;
use codex_code_bridge_protocol::BridgePayload;
use codex_code_bridge_protocol::BridgeServiceStatus;
use codex_code_bridge_protocol::BridgeSseMessage;
pub use codex_code_bridge_protocol::CLIENT_SESSION_HEADER;
use codex_code_bridge_protocol::CapabilitySet;
use codex_code_bridge_protocol::ClientMetadata;
use codex_code_bridge_protocol::ClientRole;
use codex_code_bridge_protocol::ConsoleEvent;
use codex_code_bridge_protocol::ConsoleLevel;
use codex_code_bridge_protocol::ControlCommand;
use codex_code_bridge_protocol::ControlRequestMessage;
use codex_code_bridge_protocol::ControlResponseMessage;
use codex_code_bridge_protocol::ControlResultEvent;
use codex_code_bridge_protocol::ControlStatus;
use codex_code_bridge_protocol::ErrorEvent;
use codex_code_bridge_protocol::ErrorMessage;
use codex_code_bridge_protocol::EventPublishMessage;
use codex_code_bridge_protocol::HelloMessage;
use codex_code_bridge_protocol::HelloResponseMessage;
use codex_code_bridge_protocol::PROTOCOL_VERSION;
use codex_code_bridge_protocol::PageviewEvent;
use codex_code_bridge_protocol::ScreenshotEvent;
use codex_code_bridge_protocol::ScreenshotPayload;
use codex_code_bridge_protocol::ScreenshotRequestMessage;
use codex_code_bridge_protocol::ScreenshotResponseMessage;
use codex_code_bridge_protocol::SubscribeMessage;
use codex_code_bridge_protocol::SubscriptionFilter;
use codex_code_bridge_protocol::validate_descriptor;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::StatusCode;
use std::path::Path;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum CodeBridgeClientError {
    #[error("failed to read Code Bridge descriptor {path}: {source}")]
    ReadDescriptor {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse Code Bridge descriptor {path}: {source}")]
    ParseDescriptor {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid Code Bridge descriptor: {0:?}")]
    InvalidDescriptor(codex_code_bridge_protocol::ValidationError),

    #[error("unsupported Code Bridge endpoint")]
    UnsupportedEndpoint,

    #[error("invalid Code Bridge endpoint URL: {0}")]
    InvalidEndpointUrl(#[from] url::ParseError),

    #[error("Code Bridge HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Code Bridge service returned HTTP {0}")]
    HttpStatus(StatusCode),

    #[error("Code Bridge hello response did not include a client session token")]
    MissingSessionToken,

    #[error("Code Bridge hello response rejected the client: {0}")]
    HelloRejected(String),

    #[error("Code Bridge SSE stream ended")]
    StreamEnded,

    #[error("Code Bridge SSE stream error: {0}")]
    EventStream(#[from] eventsource_stream::EventStreamError<reqwest::Error>),

    #[error("failed to parse Code Bridge SSE message: {0}")]
    ParseSse(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct CodeBridgeClient {
    http: reqwest::Client,
    endpoint_url: Url,
    auth_secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotRequest {
    pub request_id: String,
    pub target_client_id: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotResponse {
    pub request_id: String,
    pub status: ControlStatus,
    pub screenshot: Option<ScreenshotPayload>,
    pub error: Option<ErrorMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlRequest {
    pub request_id: String,
    pub target_client_id: String,
    pub command: ControlCommand,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlResponse {
    pub request_id: String,
    pub status: ControlStatus,
    pub summary: String,
    pub error: Option<ErrorMessage>,
}

impl CodeBridgeClient {
    pub fn from_descriptor(descriptor: BridgeDescriptor) -> Result<Self, CodeBridgeClientError> {
        validate_descriptor(&descriptor).map_err(CodeBridgeClientError::InvalidDescriptor)?;
        let BridgeEndpoint::LoopbackHttp { url } = descriptor.endpoint else {
            return Err(CodeBridgeClientError::UnsupportedEndpoint);
        };
        Ok(Self {
            http: reqwest::Client::new(),
            endpoint_url: Url::parse(&url)?,
            auth_secret: descriptor.auth_secret,
        })
    }

    pub fn from_descriptor_path(path: &Path) -> Result<Self, CodeBridgeClientError> {
        let raw = std::fs::read(path).map_err(|source| CodeBridgeClientError::ReadDescriptor {
            path: path.display().to_string(),
            source,
        })?;
        let descriptor = serde_json::from_slice(&raw).map_err(|source| {
            CodeBridgeClientError::ParseDescriptor {
                path: path.display().to_string(),
                source,
            }
        })?;
        Self::from_descriptor(descriptor)
    }

    pub fn endpoint_url(&self) -> &str {
        self.endpoint_url.as_str()
    }

    pub async fn status(&self) -> Result<BridgeServiceStatus, CodeBridgeClientError> {
        let response = self
            .http
            .get(self.endpoint_path(&["status"]))
            .bearer_auth(&self.auth_secret)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(CodeBridgeClientError::HttpStatus(response.status()));
        }
        Ok(response.json::<BridgeServiceStatus>().await?)
    }

    pub async fn hello(
        &self,
        client_id: impl Into<String>,
        role: ClientRole,
        requested_capabilities: CapabilitySet,
        metadata: ClientMetadata,
    ) -> Result<CodeBridgeSession, CodeBridgeClientError> {
        let client_id = client_id.into();
        let payload = self
            .post_payload(
                None,
                BridgeEnvelope {
                    protocol_version: PROTOCOL_VERSION.to_string(),
                    message_id: format!("hello-{client_id}"),
                    timestamp_unix_ms: now_unix_ms(),
                    payload: BridgePayload::Hello(HelloMessage {
                        client_id: client_id.clone(),
                        role,
                        auth: AuthProof::LocalSecret {
                            secret: self.auth_secret.clone(),
                        },
                        requested_capabilities,
                        metadata,
                    }),
                },
            )
            .await?;
        let BridgePayload::HelloResponse(response) = payload else {
            return Err(CodeBridgeClientError::HelloRejected(format!(
                "unexpected response payload {payload:?}"
            )));
        };
        session_from_hello_response(client_id, response)
    }

    pub async fn post(
        &self,
        session: &CodeBridgeSession,
        envelope: BridgeEnvelope,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        self.post_payload(Some(session), envelope).await
    }

    pub async fn subscribe(
        &self,
        session: &CodeBridgeSession,
        filter: SubscriptionFilter,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        self.post(
            session,
            envelope(
                format!("subscribe-{}", session.client_id),
                BridgePayload::Subscribe(SubscribeMessage {
                    subscriber_id: session.client_id.clone(),
                    filter,
                }),
            ),
        )
        .await
    }

    pub async fn publish_console(
        &self,
        session: &CodeBridgeSession,
        event_id: impl Into<String>,
        level: ConsoleLevel,
        text: impl Into<String>,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        self.publish_event(
            session,
            event_id,
            BridgeEvent::Console(ConsoleEvent {
                level,
                text: text.into(),
            }),
        )
        .await
    }

    pub async fn publish_error(
        &self,
        session: &CodeBridgeSession,
        event_id: impl Into<String>,
        event: ErrorEvent,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        self.publish_event(session, event_id, BridgeEvent::Error(event))
            .await
    }

    pub async fn publish_pageview(
        &self,
        session: &CodeBridgeSession,
        event_id: impl Into<String>,
        event: PageviewEvent,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        self.publish_event(session, event_id, BridgeEvent::Pageview(event))
            .await
    }

    pub async fn publish_screenshot_event(
        &self,
        session: &CodeBridgeSession,
        event_id: impl Into<String>,
        event: ScreenshotEvent,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        self.publish_event(session, event_id, BridgeEvent::Screenshot(event))
            .await
    }

    pub async fn publish_control_result(
        &self,
        session: &CodeBridgeSession,
        event_id: impl Into<String>,
        event: ControlResultEvent,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        self.publish_event(session, event_id, BridgeEvent::ControlResult(event))
            .await
    }

    pub async fn request_screenshot(
        &self,
        session: &CodeBridgeSession,
        request: ScreenshotRequest,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        let message_id = format!("screenshot-request-{}", request.request_id);
        self.post(
            session,
            envelope(
                message_id,
                BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                    request_id: request.request_id,
                    requester_client_id: session.client_id.clone(),
                    target_client_id: request.target_client_id,
                    timeout_ms: request.timeout_ms,
                }),
            ),
        )
        .await
    }

    pub async fn respond_screenshot(
        &self,
        session: &CodeBridgeSession,
        response: ScreenshotResponse,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        let message_id = format!("screenshot-response-{}", response.request_id);
        self.post(
            session,
            envelope(
                message_id,
                BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                    request_id: response.request_id,
                    responding_client_id: session.client_id.clone(),
                    status: response.status,
                    screenshot: response.screenshot,
                    error: response.error,
                }),
            ),
        )
        .await
    }

    pub async fn request_control(
        &self,
        session: &CodeBridgeSession,
        request: ControlRequest,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        let message_id = format!("control-request-{}", request.request_id);
        self.post(
            session,
            envelope(
                message_id,
                BridgePayload::ControlRequest(ControlRequestMessage {
                    request_id: request.request_id,
                    requester_client_id: session.client_id.clone(),
                    target_client_id: request.target_client_id,
                    command: request.command,
                    timeout_ms: request.timeout_ms,
                }),
            ),
        )
        .await
    }

    pub async fn respond_control(
        &self,
        session: &CodeBridgeSession,
        response: ControlResponse,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        self.respond_control_payload(session, response, None).await
    }

    pub async fn respond_control_with_result(
        &self,
        session: &CodeBridgeSession,
        response: ControlResponse,
        result: serde_json::Value,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        self.respond_control_payload(session, response, Some(result))
            .await
    }

    async fn respond_control_payload(
        &self,
        session: &CodeBridgeSession,
        response: ControlResponse,
        result: Option<serde_json::Value>,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        let message_id = format!("control-response-{}", response.request_id);
        self.post(
            session,
            envelope(
                message_id,
                BridgePayload::ControlResponse(ControlResponseMessage {
                    request_id: response.request_id,
                    responding_client_id: session.client_id.clone(),
                    status: response.status,
                    summary: response.summary,
                    result,
                    error: response.error,
                }),
            ),
        )
        .await
    }

    pub async fn events(
        &self,
        session: &CodeBridgeSession,
        last_event_id: u64,
    ) -> Result<CodeBridgeEventStream, CodeBridgeClientError> {
        let encoded_client_id = urlencoding::encode(&session.client_id);
        let response = self
            .http
            .get(self.endpoint_path_with_encoded_tail(&["events"], encoded_client_id.as_ref()))
            .bearer_auth(&self.auth_secret)
            .header(CLIENT_SESSION_HEADER, session.session_token.as_str())
            .header("last-event-id", last_event_id.to_string())
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(CodeBridgeClientError::HttpStatus(response.status()));
        }
        Ok(CodeBridgeEventStream {
            inner: Box::pin(response.bytes_stream().eventsource()),
        })
    }

    async fn post_payload(
        &self,
        session: Option<&CodeBridgeSession>,
        envelope: BridgeEnvelope,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        let mut request = self
            .http
            .post(self.endpoint_path(&["message"]))
            .bearer_auth(&self.auth_secret);
        if let Some(session) = session {
            request = request.header(CLIENT_SESSION_HEADER, session.session_token.as_str());
        }
        let response = request.json(&envelope).send().await?;
        if !response.status().is_success() {
            return Err(CodeBridgeClientError::HttpStatus(response.status()));
        }
        Ok(response.json::<BridgeMessageResponse>().await?.payload)
    }

    async fn publish_event(
        &self,
        session: &CodeBridgeSession,
        event_id: impl Into<String>,
        event: BridgeEvent,
    ) -> Result<BridgePayload, CodeBridgeClientError> {
        let event_id = event_id.into();
        self.post(
            session,
            envelope(
                event_id.clone(),
                BridgePayload::Event(EventPublishMessage {
                    client_id: session.client_id.clone(),
                    event_id,
                    event,
                }),
            ),
        )
        .await
    }

    fn endpoint_path(&self, segments: &[&str]) -> Url {
        let mut url = self.endpoint_url.clone();
        if let Ok(mut path_segments) = url.path_segments_mut() {
            path_segments.pop_if_empty();
            for segment in segments {
                path_segments.push(segment);
            }
        }
        url
    }

    fn endpoint_path_with_encoded_tail(&self, segments: &[&str], encoded_tail: &str) -> String {
        let mut url = self.endpoint_path(segments).to_string();
        let suffix_start = query_or_fragment_start(&url).unwrap_or(url.len());
        let suffix = url.split_off(suffix_start);
        format!("{}/{encoded_tail}{suffix}", url.trim_end_matches('/'))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBridgeSession {
    pub client_id: String,
    pub session_token: String,
    pub granted_capabilities: CapabilitySet,
}

pub struct CodeBridgeEventStream {
    inner: futures::stream::BoxStream<
        'static,
        Result<eventsource_stream::Event, eventsource_stream::EventStreamError<reqwest::Error>>,
    >,
}

impl CodeBridgeEventStream {
    pub async fn next_message(&mut self) -> Result<BridgeSseMessage, CodeBridgeClientError> {
        let event = self
            .inner
            .next()
            .await
            .ok_or(CodeBridgeClientError::StreamEnded)??;
        Ok(serde_json::from_str(&event.data)?)
    }
}

pub fn envelope(message_id: impl Into<String>, payload: BridgePayload) -> BridgeEnvelope {
    BridgeEnvelope {
        protocol_version: PROTOCOL_VERSION.to_string(),
        message_id: message_id.into(),
        timestamp_unix_ms: now_unix_ms(),
        payload,
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn query_or_fragment_start(url: &str) -> Option<usize> {
    [url.find('?'), url.find('#')].into_iter().flatten().min()
}

fn session_from_hello_response(
    client_id: String,
    response: HelloResponseMessage,
) -> Result<CodeBridgeSession, CodeBridgeClientError> {
    if !response.accepted {
        let message = response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "hello was not accepted".to_string());
        return Err(CodeBridgeClientError::HelloRejected(message));
    }
    Ok(CodeBridgeSession {
        client_id,
        session_token: response
            .client_session_token
            .ok_or(CodeBridgeClientError::MissingSessionToken)?,
        granted_capabilities: response.granted_capabilities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use codex_code_bridge_protocol::ErrorCode;
    use codex_code_bridge_protocol::EventKind;
    use codex_code_bridge_protocol::ScreenshotMediaType;
    use codex_code_bridge_protocol::SourceKind;
    use codex_code_bridge_service::BridgeServiceConfig;
    use image::ImageBuffer;
    use image::ImageFormat;
    use image::Rgba;
    use std::time::Duration;
    use tempfile::TempDir;

    #[tokio::test]
    async fn descriptor_client_round_trips_events_screenshot_and_control() {
        let temp = TempDir::new().expect("temp home");
        let mut config = BridgeServiceConfig::new(temp.path().to_path_buf());
        config.stale_client_timeout = Duration::from_secs(30);
        config.stale_client_sweep_interval = Duration::from_secs(30);
        let service = codex_code_bridge_service::start(config)
            .await
            .expect("start service");

        let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())
            .expect("descriptor client");
        let producer = client
            .hello(
                "producer-1",
                ClientRole::Producer,
                CapabilitySet {
                    publish_events: true,
                    provide_screenshot: true,
                    provide_control: true,
                    provide_javascript_execution: true,
                    ..CapabilitySet::default()
                },
                metadata("producer"),
            )
            .await
            .expect("producer hello");
        let subscriber = client
            .hello(
                "subscriber-1",
                ClientRole::Subscriber,
                CapabilitySet {
                    subscribe_events: true,
                    request_screenshot: true,
                    request_control: true,
                    ..CapabilitySet::default()
                },
                metadata("subscriber"),
            )
            .await
            .expect("subscriber hello");

        client
            .subscribe(
                &subscriber,
                SubscriptionFilter {
                    levels: Vec::new(),
                    event_kinds: vec![EventKind::Console],
                    client_ids: vec![producer.client_id.clone()],
                },
            )
            .await
            .expect("subscribe");
        client
            .publish_console(&producer, "event-1", ConsoleLevel::Info, "hello bridge")
            .await
            .expect("publish event");
        let mut subscriber_events = client.events(&subscriber, 0).await.expect("events");
        let message = next_test_message(&mut subscriber_events, "event message").await;
        let event_sequence = message.sequence;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::Event(EventPublishMessage { event_id, .. }) if event_id == "event-1"
        ));
        drop(subscriber_events);

        client
            .request_screenshot(
                &subscriber,
                ScreenshotRequest {
                    request_id: "shot-1".to_string(),
                    target_client_id: producer.client_id.clone(),
                    timeout_ms: 1_000,
                },
            )
            .await
            .expect("screenshot request");
        let mut producer_events = client.events(&producer, 0).await.expect("producer events");
        let message = next_test_message(&mut producer_events, "screenshot request message").await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ScreenshotRequest(ScreenshotRequestMessage { request_id, .. })
                if request_id == "shot-1"
        ));
        client
            .respond_screenshot(
                &producer,
                ScreenshotResponse {
                    request_id: "shot-1".to_string(),
                    status: ControlStatus::Ok,
                    screenshot: Some(ScreenshotPayload {
                        width: 1,
                        height: 1,
                        media_type: ScreenshotMediaType::Png,
                        data_base64: "iVBORw0KGgo=".to_string(),
                    }),
                    error: None,
                },
            )
            .await
            .expect("screenshot response");
        let mut subscriber_events = client
            .events(&subscriber, event_sequence)
            .await
            .expect("subscriber replay");
        let message =
            next_test_message(&mut subscriber_events, "screenshot response message").await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ScreenshotResponse(ScreenshotResponseMessage { request_id, .. })
                if request_id == "shot-1"
        ));

        client
            .request_control(
                &subscriber,
                ControlRequest {
                    request_id: "js-1".to_string(),
                    target_client_id: producer.client_id.clone(),
                    command: ControlCommand::ExecuteJavascript {
                        code: "window.location.href".to_string(),
                    },
                    timeout_ms: 1_000,
                },
            )
            .await
            .expect("control request");
        let message = next_test_message(&mut producer_events, "control request message").await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ControlRequest(ControlRequestMessage { request_id, .. })
                if request_id == "js-1"
        ));
        client
            .respond_control_with_result(
                &producer,
                ControlResponse {
                    request_id: "js-1".to_string(),
                    status: ControlStatus::Ok,
                    summary: "https://example.test/page".to_string(),
                    error: None,
                },
                serde_json::json!("https://example.test/page"),
            )
            .await
            .expect("control response");
        let message = next_test_message(&mut subscriber_events, "control response message").await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ControlResponse(ControlResponseMessage { request_id, result, .. })
                if request_id == "js-1"
                    && result == Some(serde_json::json!("https://example.test/page"))
        ));

        service.shutdown().await;
    }

    #[tokio::test]
    async fn descriptor_client_reads_service_status() {
        let temp = TempDir::new().expect("temp home");
        let mut config = BridgeServiceConfig::new(temp.path().to_path_buf());
        config.stale_client_timeout = Duration::from_secs(30);
        config.stale_client_sweep_interval = Duration::from_secs(30);
        let service = codex_code_bridge_service::start(config)
            .await
            .expect("start service");

        let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())
            .expect("descriptor client");
        let status = client.status().await.expect("status");

        assert_eq!(status.protocol_version, PROTOCOL_VERSION);
        assert_eq!(status.connected_producer_count, 0);
        assert_eq!(status.connected_subscriber_count, 0);
        assert_eq!(status.last_event_time_unix_ms, None);

        service.shutdown().await;
    }

    #[tokio::test]
    async fn descriptor_client_publishes_typed_events() {
        let temp = TempDir::new().expect("temp home");
        let mut config = BridgeServiceConfig::new(temp.path().to_path_buf());
        config.stale_client_timeout = Duration::from_secs(30);
        config.stale_client_sweep_interval = Duration::from_secs(30);
        let service = codex_code_bridge_service::start(config)
            .await
            .expect("start service");

        let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())
            .expect("descriptor client");
        let producer = client
            .hello(
                "producer-1",
                ClientRole::Producer,
                CapabilitySet {
                    publish_events: true,
                    provide_screenshot: true,
                    ..CapabilitySet::default()
                },
                metadata("producer"),
            )
            .await
            .expect("producer hello");
        let subscriber = client
            .hello(
                "subscriber-1",
                ClientRole::Subscriber,
                CapabilitySet {
                    subscribe_events: true,
                    ..CapabilitySet::default()
                },
                metadata("subscriber"),
            )
            .await
            .expect("subscriber hello");

        client
            .subscribe(
                &subscriber,
                SubscriptionFilter {
                    levels: Vec::new(),
                    event_kinds: vec![
                        EventKind::Error,
                        EventKind::Pageview,
                        EventKind::Screenshot,
                        EventKind::ControlResult,
                    ],
                    client_ids: vec![producer.client_id.clone()],
                },
            )
            .await
            .expect("subscribe");
        let mut subscriber_events = client.events(&subscriber, 0).await.expect("events");

        client
            .publish_error(
                &producer,
                "error-1",
                ErrorEvent {
                    message: "boom".to_string(),
                    stack: Some("stack".to_string()),
                },
            )
            .await
            .expect("publish error");
        client
            .publish_pageview(
                &producer,
                "page-1",
                PageviewEvent {
                    url: "https://example.test/page".to_string(),
                    title: Some("Example".to_string()),
                },
            )
            .await
            .expect("publish pageview");
        client
            .publish_screenshot_event(
                &producer,
                "shot-event-1",
                ScreenshotEvent {
                    screenshot_id: "shot-1".to_string(),
                    width: 1,
                    height: 1,
                    media_type: ScreenshotMediaType::Png,
                    bytes: 8,
                },
            )
            .await
            .expect("publish screenshot event");
        client
            .publish_control_result(
                &producer,
                "control-result-1",
                ControlResultEvent {
                    request_id: "js-1".to_string(),
                    status: ControlStatus::Failed,
                    summary: "failed".to_string(),
                    result: None,
                },
            )
            .await
            .expect("publish control result");

        assert!(matches!(
            next_test_message(&mut subscriber_events, "error event")
                .await
                .envelope
                .payload,
            BridgePayload::Event(EventPublishMessage {
                event: BridgeEvent::Error(ErrorEvent { message, stack }),
                ..
            }) if message == "boom" && stack.as_deref() == Some("stack")
        ));
        assert!(matches!(
            next_test_message(&mut subscriber_events, "pageview event")
                .await
                .envelope
                .payload,
            BridgePayload::Event(EventPublishMessage {
                event: BridgeEvent::Pageview(PageviewEvent { url, title }),
                ..
            }) if url == "https://example.test/page" && title.as_deref() == Some("Example")
        ));
        assert!(matches!(
            next_test_message(&mut subscriber_events, "screenshot event")
                .await
                .envelope
                .payload,
            BridgePayload::Event(EventPublishMessage {
                event: BridgeEvent::Screenshot(ScreenshotEvent { screenshot_id, bytes, .. }),
                ..
            }) if screenshot_id == "shot-1" && bytes == 8
        ));
        assert!(matches!(
            next_test_message(&mut subscriber_events, "control result event")
                .await
                .envelope
                .payload,
            BridgePayload::Event(EventPublishMessage {
                event: BridgeEvent::ControlResult(ControlResultEvent { request_id, status, summary, result }),
                ..
            }) if request_id == "js-1"
                && status == ControlStatus::Failed
                && summary == "failed"
                && result.is_none()
        ));

        service.shutdown().await;
    }

    #[tokio::test]
    async fn typed_helpers_surface_capability_denials() {
        let temp = TempDir::new().expect("temp home");
        let mut config = BridgeServiceConfig::new(temp.path().to_path_buf());
        config.stale_client_timeout = Duration::from_secs(30);
        config.stale_client_sweep_interval = Duration::from_secs(30);
        let service = codex_code_bridge_service::start(config)
            .await
            .expect("start service");

        let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())
            .expect("descriptor client");
        let producer = client
            .hello(
                "producer-1",
                ClientRole::Producer,
                CapabilitySet {
                    provide_screenshot: true,
                    ..CapabilitySet::default()
                },
                metadata("producer"),
            )
            .await
            .expect("producer hello");
        let subscriber = client
            .hello(
                "subscriber-1",
                ClientRole::Subscriber,
                CapabilitySet {
                    subscribe_events: true,
                    ..CapabilitySet::default()
                },
                metadata("subscriber"),
            )
            .await
            .expect("subscriber hello");

        let payload = client
            .request_screenshot(
                &subscriber,
                ScreenshotRequest {
                    request_id: "shot-1".to_string(),
                    target_client_id: producer.client_id.clone(),
                    timeout_ms: 1_000,
                },
            )
            .await
            .expect("capability response");
        assert!(matches!(
            payload,
            BridgePayload::Error(ErrorMessage {
                code: ErrorCode::CapabilityDenied,
                ..
            })
        ));

        service.shutdown().await;
    }

    #[tokio::test]
    async fn browser_fixture_round_trips_nonblank_screenshot_and_control() {
        // This is a browser-app protocol fixture, not DOM/browser automation. It
        // proves the local bridge can carry browser-shaped events, a nonblank
        // screenshot payload, and bounded JavaScript control responses end to end.
        let temp = TempDir::new().expect("temp home");
        let mut config = BridgeServiceConfig::new(temp.path().to_path_buf());
        config.stale_client_timeout = Duration::from_secs(30);
        config.stale_client_sweep_interval = Duration::from_secs(30);
        let service = codex_code_bridge_service::start(config)
            .await
            .expect("start service");

        let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())
            .expect("descriptor client");
        let browser = client
            .hello(
                "browser-fixture-1",
                ClientRole::Producer,
                CapabilitySet {
                    publish_events: true,
                    provide_screenshot: true,
                    provide_control: true,
                    provide_javascript_execution: true,
                    ..CapabilitySet::default()
                },
                ClientMetadata {
                    source_kind: SourceKind::BrowserApp,
                    label: Some("browser witness".to_string()),
                    ..ClientMetadata::default()
                },
            )
            .await
            .expect("browser hello");
        let consumer = client
            .hello(
                "consumer-1",
                ClientRole::Subscriber,
                CapabilitySet {
                    subscribe_events: true,
                    request_screenshot: true,
                    request_control: true,
                    ..CapabilitySet::default()
                },
                metadata("consumer"),
            )
            .await
            .expect("consumer hello");

        client
            .subscribe(
                &consumer,
                SubscriptionFilter {
                    levels: Vec::new(),
                    event_kinds: vec![EventKind::Console, EventKind::Pageview],
                    client_ids: vec![browser.client_id.clone()],
                },
            )
            .await
            .expect("subscribe");
        let mut consumer_events = client.events(&consumer, 0).await.expect("consumer events");

        client
            .publish_pageview(
                &browser,
                "pageview-1",
                PageviewEvent {
                    url: "http://127.0.0.1/browser-fixture".to_string(),
                    title: Some("Code Bridge Browser Fixture".to_string()),
                },
            )
            .await
            .expect("publish pageview");
        client
            .publish_console(&browser, "console-1", ConsoleLevel::Info, "fixture ready")
            .await
            .expect("publish console");

        assert!(matches!(
            next_test_message(&mut consumer_events, "pageview event")
                .await
                .envelope
                .payload,
            BridgePayload::Event(EventPublishMessage {
                event: BridgeEvent::Pageview(PageviewEvent { url, title }),
                ..
            }) if url == "http://127.0.0.1/browser-fixture"
                && title.as_deref() == Some("Code Bridge Browser Fixture")
        ));
        let event_sequence = next_test_message(&mut consumer_events, "console event")
            .await
            .sequence;
        drop(consumer_events);

        client
            .request_screenshot(
                &consumer,
                ScreenshotRequest {
                    request_id: "browser-shot-1".to_string(),
                    target_client_id: browser.client_id.clone(),
                    timeout_ms: 1_000,
                },
            )
            .await
            .expect("request screenshot");
        let mut browser_events = client.events(&browser, 0).await.expect("browser events");
        assert!(matches!(
            next_test_message(&mut browser_events, "screenshot request")
                .await
                .envelope
                .payload,
            BridgePayload::ScreenshotRequest(ScreenshotRequestMessage { request_id, .. })
                if request_id == "browser-shot-1"
        ));

        let screenshot = browser_fixture_screenshot();
        assert_nonblank_png(&screenshot);
        client
            .respond_screenshot(
                &browser,
                ScreenshotResponse {
                    request_id: "browser-shot-1".to_string(),
                    status: ControlStatus::Ok,
                    screenshot: Some(screenshot),
                    error: None,
                },
            )
            .await
            .expect("respond screenshot");
        let mut consumer_events = client
            .events(&consumer, event_sequence)
            .await
            .expect("consumer replay");
        let response = next_test_message(&mut consumer_events, "screenshot response").await;
        let BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
            request_id,
            screenshot: Some(screenshot),
            ..
        }) = response.envelope.payload
        else {
            panic!("expected screenshot response");
        };
        assert_eq!(request_id, "browser-shot-1");
        assert_nonblank_png(&screenshot);

        client
            .request_control(
                &consumer,
                ControlRequest {
                    request_id: "browser-js-1".to_string(),
                    target_client_id: browser.client_id.clone(),
                    command: ControlCommand::ExecuteJavascript {
                        code: "window.location.href".to_string(),
                    },
                    timeout_ms: 1_000,
                },
            )
            .await
            .expect("request control");
        assert!(matches!(
            next_test_message(&mut browser_events, "control request")
                .await
                .envelope
                .payload,
            BridgePayload::ControlRequest(ControlRequestMessage { request_id, command, .. })
                if request_id == "browser-js-1"
                    && matches!(&command, ControlCommand::ExecuteJavascript { code } if code == "window.location.href")
        ));
        client
            .respond_control_with_result(
                &browser,
                ControlResponse {
                    request_id: "browser-js-1".to_string(),
                    status: ControlStatus::Ok,
                    summary: "http://127.0.0.1/browser-fixture".to_string(),
                    error: None,
                },
                serde_json::json!("http://127.0.0.1/browser-fixture"),
            )
            .await
            .expect("respond control");
        assert!(matches!(
            next_test_message(&mut consumer_events, "control response")
                .await
                .envelope
                .payload,
            BridgePayload::ControlResponse(ControlResponseMessage { request_id, summary, result, .. })
                if request_id == "browser-js-1"
                    && summary == "http://127.0.0.1/browser-fixture"
                    && result == Some(serde_json::json!("http://127.0.0.1/browser-fixture"))
        ));

        service.shutdown().await;
    }

    #[tokio::test]
    async fn events_encodes_path_like_client_id() {
        let temp = TempDir::new().expect("temp home");
        let mut config = BridgeServiceConfig::new(temp.path().to_path_buf());
        config.stale_client_timeout = Duration::from_secs(30);
        config.stale_client_sweep_interval = Duration::from_secs(30);
        let service = codex_code_bridge_service::start(config)
            .await
            .expect("start service");

        let client = CodeBridgeClient::from_descriptor_path(service.descriptor_path())
            .expect("descriptor client");
        let producer = client
            .hello(
                "producer/path like?100%",
                ClientRole::Producer,
                CapabilitySet {
                    publish_events: true,
                    ..CapabilitySet::default()
                },
                metadata("producer"),
            )
            .await
            .expect("producer hello");
        let subscriber = client
            .hello(
                "subscriber/path like?100%",
                ClientRole::Subscriber,
                CapabilitySet {
                    subscribe_events: true,
                    ..CapabilitySet::default()
                },
                metadata("subscriber"),
            )
            .await
            .expect("subscriber hello");

        client
            .subscribe(
                &subscriber,
                SubscriptionFilter {
                    levels: Vec::new(),
                    event_kinds: vec![EventKind::Console],
                    client_ids: vec![producer.client_id.clone()],
                },
            )
            .await
            .expect("subscribe");
        client
            .publish_console(&producer, "event-1", ConsoleLevel::Info, "hello bridge")
            .await
            .expect("publish event");

        let mut subscriber_events = client.events(&subscriber, 0).await.expect("events");
        let message = next_test_message(&mut subscriber_events, "event message").await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::Event(EventPublishMessage { event_id, .. }) if event_id == "event-1"
        ));

        service.shutdown().await;
    }

    #[test]
    fn endpoint_paths_preserve_base_path_and_encode_segments() {
        let cases = [
            (
                "http://127.0.0.1:12345",
                "http://127.0.0.1:12345/message",
                "http://127.0.0.1:12345/events/client%2Fpath%20like%3F100%25%2F..%2F.%2F..",
            ),
            (
                "http://127.0.0.1:12345/",
                "http://127.0.0.1:12345/message",
                "http://127.0.0.1:12345/events/client%2Fpath%20like%3F100%25%2F..%2F.%2F..",
            ),
            (
                "http://127.0.0.1:12345/bridge",
                "http://127.0.0.1:12345/bridge/message",
                "http://127.0.0.1:12345/bridge/events/client%2Fpath%20like%3F100%25%2F..%2F.%2F..",
            ),
            (
                "http://127.0.0.1:12345/bridge/",
                "http://127.0.0.1:12345/bridge/message",
                "http://127.0.0.1:12345/bridge/events/client%2Fpath%20like%3F100%25%2F..%2F.%2F..",
            ),
            (
                "http://127.0.0.1:12345/bridge?token=local#ignored",
                "http://127.0.0.1:12345/bridge/message?token=local#ignored",
                "http://127.0.0.1:12345/bridge/events/client%2Fpath%20like%3F100%25%2F..%2F.%2F..?token=local#ignored",
            ),
            (
                "http://127.0.0.1:12345/bridge%2Fv1?token=local#ignored",
                "http://127.0.0.1:12345/bridge%2Fv1/message?token=local#ignored",
                "http://127.0.0.1:12345/bridge%2Fv1/events/client%2Fpath%20like%3F100%25%2F..%2F.%2F..?token=local#ignored",
            ),
        ];

        for (endpoint_url, message_url, events_url) in cases {
            let client = client_for_endpoint(endpoint_url);
            assert_eq!(client.endpoint_path(&["message"]).as_str(), message_url);
            assert_eq!(
                client.endpoint_path_with_encoded_tail(
                    &["events"],
                    urlencoding::encode("client/path like?100%/.././..").as_ref(),
                ),
                events_url
            );
        }
    }

    fn metadata(label: &str) -> ClientMetadata {
        ClientMetadata {
            source_kind: SourceKind::TestFixture,
            label: Some(label.to_string()),
            ..ClientMetadata::default()
        }
    }

    async fn next_test_message(
        stream: &mut CodeBridgeEventStream,
        context: &str,
    ) -> BridgeSseMessage {
        tokio::time::timeout(Duration::from_secs(2), stream.next_message())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {context}"))
            .unwrap_or_else(|err| panic!("failed to read {context}: {err}"))
    }

    fn client_for_endpoint(endpoint_url: &str) -> CodeBridgeClient {
        CodeBridgeClient::from_descriptor(BridgeDescriptor {
            protocol_version: PROTOCOL_VERSION.to_string(),
            endpoint: BridgeEndpoint::LoopbackHttp {
                url: endpoint_url.to_string(),
            },
            auth_secret: "secret".to_string(),
            pid: None,
        })
        .expect("client from descriptor")
    }

    fn browser_fixture_screenshot() -> ScreenshotPayload {
        let image = ImageBuffer::from_fn(4, 3, |x, y| {
            if x == 0 && y == 0 {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([40 + (x as u8 * 30), 90 + (y as u8 * 20), 180, 255])
            }
        });
        let mut bytes = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("encode browser fixture screenshot");
        ScreenshotPayload {
            width: 4,
            height: 3,
            media_type: ScreenshotMediaType::Png,
            data_base64: BASE64_STANDARD.encode(bytes),
        }
    }

    fn assert_nonblank_png(screenshot: &ScreenshotPayload) {
        assert_eq!(screenshot.media_type, ScreenshotMediaType::Png);
        let bytes = BASE64_STANDARD
            .decode(&screenshot.data_base64)
            .expect("decode screenshot");
        let decoded = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
            .expect("decode screenshot png")
            .into_rgba8();
        assert_eq!(decoded.width(), screenshot.width);
        assert_eq!(decoded.height(), screenshot.height);
        let first_pixel = decoded.get_pixel(0, 0);
        assert!(decoded.pixels().any(|pixel| pixel != first_pixel));
    }
}
