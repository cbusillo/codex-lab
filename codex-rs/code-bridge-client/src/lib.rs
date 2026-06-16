use codex_code_bridge_protocol::AuthProof;
use codex_code_bridge_protocol::BridgeDescriptor;
use codex_code_bridge_protocol::BridgeEndpoint;
use codex_code_bridge_protocol::BridgeEnvelope;
use codex_code_bridge_protocol::BridgeEvent;
use codex_code_bridge_protocol::BridgeMessageResponse;
use codex_code_bridge_protocol::BridgePayload;
use codex_code_bridge_protocol::BridgeSseMessage;
pub use codex_code_bridge_protocol::CLIENT_SESSION_HEADER;
use codex_code_bridge_protocol::CapabilitySet;
use codex_code_bridge_protocol::ClientMetadata;
use codex_code_bridge_protocol::ClientRole;
use codex_code_bridge_protocol::ConsoleEvent;
use codex_code_bridge_protocol::ConsoleLevel;
use codex_code_bridge_protocol::EventPublishMessage;
use codex_code_bridge_protocol::HelloMessage;
use codex_code_bridge_protocol::HelloResponseMessage;
use codex_code_bridge_protocol::PROTOCOL_VERSION;
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
    endpoint_url: String,
    auth_secret: String,
}

impl CodeBridgeClient {
    pub fn from_descriptor(descriptor: BridgeDescriptor) -> Result<Self, CodeBridgeClientError> {
        validate_descriptor(&descriptor).map_err(CodeBridgeClientError::InvalidDescriptor)?;
        let BridgeEndpoint::LoopbackHttp { url } = descriptor.endpoint else {
            return Err(CodeBridgeClientError::UnsupportedEndpoint);
        };
        Ok(Self {
            http: reqwest::Client::new(),
            endpoint_url: url,
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
        &self.endpoint_url
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
        let event_id = event_id.into();
        self.post(
            session,
            envelope(
                event_id.clone(),
                BridgePayload::Event(EventPublishMessage {
                    client_id: session.client_id.clone(),
                    event_id,
                    event: BridgeEvent::Console(ConsoleEvent {
                        level,
                        text: text.into(),
                    }),
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
        let response = self
            .http
            .get(format!(
                "{}/events/{}",
                self.endpoint_url, session.client_id
            ))
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
            .post(format!("{}/message", self.endpoint_url))
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
    use codex_code_bridge_protocol::ControlCommand;
    use codex_code_bridge_protocol::ControlRequestMessage;
    use codex_code_bridge_protocol::ControlResponseMessage;
    use codex_code_bridge_protocol::ControlStatus;
    use codex_code_bridge_protocol::EventKind;
    use codex_code_bridge_protocol::ScreenshotMediaType;
    use codex_code_bridge_protocol::ScreenshotPayload;
    use codex_code_bridge_protocol::ScreenshotRequestMessage;
    use codex_code_bridge_protocol::ScreenshotResponseMessage;
    use codex_code_bridge_protocol::SourceKind;
    use codex_code_bridge_service::BridgeServiceConfig;
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
            .post(
                &subscriber,
                envelope(
                    "screenshot-request-1",
                    BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
                        request_id: "shot-1".to_string(),
                        requester_client_id: subscriber.client_id.clone(),
                        target_client_id: producer.client_id.clone(),
                        timeout_ms: 1_000,
                    }),
                ),
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
            .post(
                &producer,
                envelope(
                    "screenshot-response-1",
                    BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
                        request_id: "shot-1".to_string(),
                        responding_client_id: producer.client_id.clone(),
                        status: ControlStatus::Ok,
                        screenshot: Some(ScreenshotPayload {
                            width: 1,
                            height: 1,
                            media_type: ScreenshotMediaType::Png,
                            data_base64: "iVBORw0KGgo=".to_string(),
                        }),
                        error: None,
                    }),
                ),
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
            .post(
                &subscriber,
                envelope(
                    "control-request-1",
                    BridgePayload::ControlRequest(ControlRequestMessage {
                        request_id: "js-1".to_string(),
                        requester_client_id: subscriber.client_id.clone(),
                        target_client_id: producer.client_id.clone(),
                        command: ControlCommand::ExecuteJavascript {
                            code: "window.location.href".to_string(),
                        },
                        timeout_ms: 1_000,
                    }),
                ),
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
            .post(
                &producer,
                envelope(
                    "control-response-1",
                    BridgePayload::ControlResponse(ControlResponseMessage {
                        request_id: "js-1".to_string(),
                        responding_client_id: producer.client_id.clone(),
                        status: ControlStatus::Ok,
                        summary: "https://example.test/page".to_string(),
                        error: None,
                    }),
                ),
            )
            .await
            .expect("control response");
        let message = next_test_message(&mut subscriber_events, "control response message").await;
        assert!(matches!(
            message.envelope.payload,
            BridgePayload::ControlResponse(ControlResponseMessage { request_id, .. })
                if request_id == "js-1"
        ));

        service.shutdown().await;
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
}
