use serde::Deserialize;
use serde::Serialize;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;

use url::Host;
use url::Url;

pub const PROTOCOL_VERSION: &str = "code_bridge.v1";
pub const DESCRIPTOR_RELATIVE_PATH: &str = "code-bridge/descriptor.json";
pub const CLIENT_SESSION_HEADER: &str = "x-code-bridge-client-session";
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_SCREENSHOT_MESSAGE_BYTES: usize = MAX_SCREENSHOT_BYTES + MAX_MESSAGE_BYTES;
pub const MAX_RETAINED_EVENTS: usize = 500;
pub const MAX_EVENT_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_SCREENSHOT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SCREENSHOT_WIDTH: u32 = 4096;
pub const MAX_SCREENSHOT_HEIGHT: u32 = 4096;
pub const MAX_CONTROL_TIMEOUT_MS: u64 = 10_000;
pub const MAX_MODEL_VISIBLE_SUMMARY_BYTES: usize = 8 * 1024;
pub const MAX_CLIENT_LABEL_BYTES: usize = 128;
pub const MAX_PROVENANCE_URL_BYTES: usize = 512;
pub const MAX_PROVENANCE_ID_BYTES: usize = 128;
pub const MAX_PROVENANCE_ENVIRONMENT_LABEL_BYTES: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeEnvelope {
    pub protocol_version: String,
    pub message_id: String,
    pub timestamp_unix_ms: u64,
    pub payload: BridgePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeDescriptor {
    pub protocol_version: String,
    pub endpoint: BridgeEndpoint,
    pub auth_secret: String,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMessageResponse {
    pub payload: BridgePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSseMessage {
    pub sequence: u64,
    pub envelope: BridgeEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "body", rename_all = "camelCase")]
pub enum BridgeEndpoint {
    LoopbackHttp { url: String },
    UnixSocket { path: String },
    WindowsNamedPipe { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "family", content = "body", rename_all = "camelCase")]
pub enum BridgePayload {
    Hello(HelloMessage),
    HelloResponse(HelloResponseMessage),
    Heartbeat(HeartbeatMessage),
    Event(EventPublishMessage),
    Subscribe(SubscribeMessage),
    Ack(AckMessage),
    Error(ErrorMessage),
    ScreenshotRequest(ScreenshotRequestMessage),
    ScreenshotResponse(ScreenshotResponseMessage),
    ControlRequest(ControlRequestMessage),
    ControlResponse(ControlResponseMessage),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HelloMessage {
    pub client_id: String,
    pub role: ClientRole,
    pub auth: AuthProof,
    pub requested_capabilities: CapabilitySet,
    pub metadata: ClientMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HelloResponseMessage {
    pub accepted: bool,
    pub protocol_version: String,
    pub granted_capabilities: CapabilitySet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_session_token: Option<String>,
    pub limits: BridgeLimits,
    pub error: Option<ErrorMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatMessage {
    pub client_id: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EventPublishMessage {
    pub client_id: String,
    pub event_id: String,
    pub event: BridgeEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "body", rename_all = "camelCase")]
pub enum BridgeEvent {
    Console(ConsoleEvent),
    Error(ErrorEvent),
    Pageview(PageviewEvent),
    Screenshot(ScreenshotEvent),
    ControlResult(ControlResultEvent),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleEvent {
    pub level: ConsoleLevel,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConsoleLevel {
    Trace,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEvent {
    pub message: String,
    pub stack: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PageviewEvent {
    pub url: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotEvent {
    pub screenshot_id: String,
    pub width: u32,
    pub height: u32,
    pub media_type: ScreenshotMediaType,
    pub bytes: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ScreenshotMediaType {
    Png,
    Jpeg,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlResultEvent {
    pub request_id: String,
    pub status: ControlStatus,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeMessage {
    pub subscriber_id: String,
    pub filter: SubscriptionFilter,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionFilter {
    pub levels: Vec<ConsoleLevel>,
    pub event_kinds: Vec<EventKind>,
    pub client_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    Console,
    Error,
    Pageview,
    Screenshot,
    ControlResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AckMessage {
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorMessage {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    AuthRequired,
    AuthRejected,
    CapabilityDenied,
    InvalidPayload,
    PayloadTooLarge,
    Timeout,
    UnsupportedProtocolVersion,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotRequestMessage {
    pub request_id: String,
    pub requester_client_id: String,
    pub target_client_id: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotResponseMessage {
    pub request_id: String,
    pub responding_client_id: String,
    pub status: ControlStatus,
    pub screenshot: Option<ScreenshotPayload>,
    pub error: Option<ErrorMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotPayload {
    pub width: u32,
    pub height: u32,
    pub media_type: ScreenshotMediaType,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlRequestMessage {
    pub request_id: String,
    pub requester_client_id: String,
    pub target_client_id: String,
    pub command: ControlCommand,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "body", rename_all = "camelCase")]
pub enum ControlCommand {
    CaptureScreenshot,
    ExecuteJavascript { code: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlResponseMessage {
    pub request_id: String,
    pub responding_client_id: String,
    pub status: ControlStatus,
    pub summary: String,
    pub error: Option<ErrorMessage>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ControlStatus {
    Ok,
    Failed,
    TimedOut,
    Denied,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ClientRole {
    Producer,
    Subscriber,
    ServiceControl,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "body", rename_all = "camelCase")]
pub enum AuthProof {
    LocalSecret { secret: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct CapabilitySet {
    pub publish_events: bool,
    pub subscribe_events: bool,
    pub request_screenshot: bool,
    pub provide_screenshot: bool,
    pub request_control: bool,
    pub provide_control: bool,
    pub provide_javascript_execution: bool,
    pub service_control: bool,
}

impl CapabilitySet {
    pub fn for_role(&self, role: ClientRole) -> Self {
        match role {
            ClientRole::Producer => Self {
                publish_events: self.publish_events,
                provide_screenshot: self.provide_screenshot,
                provide_control: self.provide_control,
                provide_javascript_execution: self.provide_javascript_execution,
                ..Self::default()
            },
            ClientRole::Subscriber => Self {
                subscribe_events: self.subscribe_events,
                request_screenshot: self.request_screenshot,
                request_control: self.request_control,
                ..Self::default()
            },
            ClientRole::ServiceControl => Self {
                service_control: self.service_control,
                ..Self::default()
            },
        }
    }

    pub fn allows_role(&self, role: ClientRole) -> bool {
        self.for_role(role).has_any_capability()
    }

    pub fn has_cross_role_capability(&self, role: ClientRole) -> bool {
        self != &self.for_role(role)
    }

    fn has_any_capability(&self) -> bool {
        self.publish_events
            || self.subscribe_events
            || self.request_screenshot
            || self.provide_screenshot
            || self.request_control
            || self.provide_control
            || self.provide_javascript_execution
            || self.service_control
    }

    pub fn allows_event(&self, event: &BridgeEvent) -> bool {
        match event {
            BridgeEvent::Console(_)
            | BridgeEvent::Error(_)
            | BridgeEvent::Pageview(_)
            | BridgeEvent::ControlResult(_) => self.publish_events,
            BridgeEvent::Screenshot(_) => self.publish_events && self.provide_screenshot,
        }
    }

    pub fn allows_command(&self, command: &ControlCommand) -> bool {
        match command {
            ControlCommand::CaptureScreenshot => self.request_screenshot,
            ControlCommand::ExecuteJavascript { .. } => self.request_control,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClientMetadata {
    pub source_kind: SourceKind,
    pub label: Option<String>,
    pub provenance: Option<ProvenanceMetadata>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    BrowserApp,
    DesktopApp,
    Cli,
    TestFixture,
    #[default]
    Other,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceMetadata {
    pub repository_url: Option<String>,
    pub issue_or_pr_url: Option<String>,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub environment_label: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeLimits {
    pub max_message_bytes: usize,
    pub max_retained_events: usize,
    pub max_event_text_bytes: usize,
    pub max_screenshot_bytes: usize,
    pub max_screenshot_width: u32,
    pub max_screenshot_height: u32,
    pub max_control_timeout_ms: u64,
    pub max_model_visible_summary_bytes: usize,
}

impl Default for BridgeLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: MAX_MESSAGE_BYTES,
            max_retained_events: MAX_RETAINED_EVENTS,
            max_event_text_bytes: MAX_EVENT_TEXT_BYTES,
            max_screenshot_bytes: MAX_SCREENSHOT_BYTES,
            max_screenshot_width: MAX_SCREENSHOT_WIDTH,
            max_screenshot_height: MAX_SCREENSHOT_HEIGHT,
            max_control_timeout_ms: MAX_CONTROL_TIMEOUT_MS,
            max_model_visible_summary_bytes: MAX_MODEL_VISIBLE_SUMMARY_BYTES,
        }
    }
}

pub fn message_family(payload: &BridgePayload) -> &'static str {
    match payload {
        BridgePayload::Hello(_) => "hello",
        BridgePayload::HelloResponse(_) => "helloResponse",
        BridgePayload::Heartbeat(_) => "heartbeat",
        BridgePayload::Event(_) => "event",
        BridgePayload::Subscribe(_) => "subscribe",
        BridgePayload::Ack(_) => "ack",
        BridgePayload::Error(_) => "error",
        BridgePayload::ScreenshotRequest(_) => "screenshotRequest",
        BridgePayload::ScreenshotResponse(_) => "screenshotResponse",
        BridgePayload::ControlRequest(_) => "controlRequest",
        BridgePayload::ControlResponse(_) => "controlResponse",
    }
}

pub fn event_kind(event: &BridgeEvent) -> EventKind {
    match event {
        BridgeEvent::Console(_) => EventKind::Console,
        BridgeEvent::Error(_) => EventKind::Error,
        BridgeEvent::Pageview(_) => EventKind::Pageview,
        BridgeEvent::Screenshot(_) => EventKind::Screenshot,
        BridgeEvent::ControlResult(_) => EventKind::ControlResult,
    }
}

pub fn validate_event_capabilities(
    capabilities: &CapabilitySet,
    event: &BridgeEvent,
) -> Result<(), ValidationError> {
    if !capabilities.allows_event(event) {
        return Err(ValidationError::CapabilityDenied);
    }
    Ok(())
}

pub fn validate_control_capabilities(
    capabilities: &CapabilitySet,
    command: &ControlCommand,
) -> Result<(), ValidationError> {
    if !capabilities.allows_command(command) {
        return Err(ValidationError::CapabilityDenied);
    }
    Ok(())
}

pub fn validate_envelope(
    envelope: &BridgeEnvelope,
    serialized_len: usize,
) -> Result<(), ValidationError> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err(ValidationError::UnsupportedProtocolVersion);
    }
    let max_message_bytes = max_serialized_len(&envelope.payload);
    if serialized_len > max_message_bytes {
        return Err(ValidationError::PayloadTooLarge {
            limit: max_message_bytes,
            actual: serialized_len,
        });
    }
    validate_payload(&envelope.payload)
}

pub fn validate_payload(payload: &BridgePayload) -> Result<(), ValidationError> {
    match payload {
        BridgePayload::Hello(message) => {
            validate_auth(&message.auth)?;
            validate_client_metadata(&message.metadata)?;
            if !message.requested_capabilities.allows_role(message.role) {
                return Err(ValidationError::CapabilityDenied);
            }
            if message
                .requested_capabilities
                .has_cross_role_capability(message.role)
            {
                return Err(ValidationError::CapabilityDenied);
            }
            Ok(())
        }
        BridgePayload::HelloResponse(message) => {
            if message.protocol_version != PROTOCOL_VERSION {
                return Err(ValidationError::UnsupportedProtocolVersion);
            }
            Ok(())
        }
        BridgePayload::Event(message) => validate_event(&message.event),
        BridgePayload::ScreenshotRequest(message) => validate_timeout(message.timeout_ms),
        BridgePayload::ScreenshotResponse(message) => {
            if let Some(screenshot) = &message.screenshot {
                validate_screenshot(screenshot)?;
            }
            Ok(())
        }
        BridgePayload::ControlRequest(message) => {
            validate_timeout(message.timeout_ms)?;
            validate_control_command(&message.command)
        }
        BridgePayload::ControlResponse(message) => validate_summary(&message.summary),
        BridgePayload::Heartbeat(_)
        | BridgePayload::Subscribe(_)
        | BridgePayload::Ack(_)
        | BridgePayload::Error(_) => Ok(()),
    }
}

pub fn validate_descriptor(descriptor: &BridgeDescriptor) -> Result<(), ValidationError> {
    if descriptor.protocol_version != PROTOCOL_VERSION {
        return Err(ValidationError::UnsupportedProtocolVersion);
    }
    if descriptor.auth_secret.is_empty() {
        return Err(ValidationError::AuthRequired);
    }
    validate_endpoint(&descriptor.endpoint)
}

fn max_serialized_len(payload: &BridgePayload) -> usize {
    match payload {
        BridgePayload::ScreenshotResponse(message) if message.screenshot.is_some() => {
            MAX_SCREENSHOT_MESSAGE_BYTES
        }
        _ => MAX_MESSAGE_BYTES,
    }
}

fn validate_endpoint(endpoint: &BridgeEndpoint) -> Result<(), ValidationError> {
    match endpoint {
        BridgeEndpoint::LoopbackHttp { url } => {
            let parsed = Url::parse(url).map_err(|_| ValidationError::InvalidEndpoint)?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(ValidationError::InvalidEndpoint);
            }
            let Some(host) = parsed.host_str() else {
                return Err(ValidationError::InvalidEndpoint);
            };
            if !matches!(host, "127.0.0.1" | "::1" | "[::1]") {
                return Err(ValidationError::InvalidEndpoint);
            }
            if parsed.port().is_none() {
                return Err(ValidationError::InvalidEndpoint);
            }
            Ok(())
        }
        BridgeEndpoint::UnixSocket { path } => {
            if path.is_empty() || !path.starts_with('/') {
                return Err(ValidationError::InvalidEndpoint);
            }
            Ok(())
        }
        BridgeEndpoint::WindowsNamedPipe { name } => {
            if name.is_empty() {
                return Err(ValidationError::InvalidEndpoint);
            }
            Ok(())
        }
    }
}

fn validate_event(event: &BridgeEvent) -> Result<(), ValidationError> {
    match event {
        BridgeEvent::Console(event) => validate_text(&event.text, MAX_EVENT_TEXT_BYTES),
        BridgeEvent::Error(event) => {
            validate_text(&event.message, MAX_EVENT_TEXT_BYTES)?;
            if let Some(stack) = &event.stack {
                validate_text(stack, MAX_EVENT_TEXT_BYTES)?;
            }
            Ok(())
        }
        BridgeEvent::Pageview(event) => {
            validate_text(&event.url, MAX_EVENT_TEXT_BYTES)?;
            if let Some(title) = &event.title {
                validate_text(title, MAX_EVENT_TEXT_BYTES)?;
            }
            Ok(())
        }
        BridgeEvent::Screenshot(event) => {
            validate_screenshot_bounds(event.width, event.height, event.bytes)
        }
        BridgeEvent::ControlResult(event) => validate_summary(&event.summary),
    }
}

fn validate_client_metadata(metadata: &ClientMetadata) -> Result<(), ValidationError> {
    if let Some(label) = &metadata.label {
        validate_text(label, MAX_CLIENT_LABEL_BYTES)?;
    }
    if let Some(provenance) = &metadata.provenance {
        validate_provenance_metadata(provenance)?;
    }
    Ok(())
}

fn validate_provenance_metadata(provenance: &ProvenanceMetadata) -> Result<(), ValidationError> {
    if let Some(repository_url) = &provenance.repository_url {
        validate_provenance_url(repository_url)?;
    }
    if let Some(issue_or_pr_url) = &provenance.issue_or_pr_url {
        validate_provenance_url(issue_or_pr_url)?;
    }
    if let Some(request_id) = &provenance.request_id {
        validate_provenance_token(request_id, MAX_PROVENANCE_ID_BYTES)?;
    }
    if let Some(trace_id) = &provenance.trace_id {
        validate_provenance_token(trace_id, MAX_PROVENANCE_ID_BYTES)?;
    }
    if let Some(environment_label) = &provenance.environment_label {
        validate_provenance_token(environment_label, MAX_PROVENANCE_ENVIRONMENT_LABEL_BYTES)?;
    }
    Ok(())
}

fn validate_provenance_url(url: &str) -> Result<(), ValidationError> {
    validate_text(url, MAX_PROVENANCE_URL_BYTES)?;
    let parsed = Url::parse(url).map_err(|_| ValidationError::InvalidProvenance)?;
    if parsed.scheme() != "https" {
        return Err(ValidationError::InvalidProvenance);
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err(ValidationError::InvalidProvenance);
    }
    if parsed.port().is_some() || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ValidationError::InvalidProvenance);
    }
    let Some(host) = parsed.host() else {
        return Err(ValidationError::InvalidProvenance);
    };
    match host {
        Host::Domain(domain) if domain.eq_ignore_ascii_case("localhost") => {
            return Err(ValidationError::InvalidProvenance);
        }
        Host::Ipv4(address) if is_private_provenance_ip(IpAddr::V4(address)) => {
            return Err(ValidationError::InvalidProvenance);
        }
        Host::Ipv6(address) if is_private_provenance_ip(IpAddr::V6(address)) => {
            return Err(ValidationError::InvalidProvenance);
        }
        Host::Domain(_) | Host::Ipv4(_) | Host::Ipv6(_) => {}
    }
    Ok(())
}

fn validate_provenance_token(token: &str, limit: usize) -> Result<(), ValidationError> {
    validate_text(token, limit)?;
    if token.is_empty()
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(ValidationError::InvalidProvenance);
    }
    Ok(())
}

fn is_private_provenance_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_private_provenance_ipv4(address),
        IpAddr::V6(address) => is_private_provenance_ipv6(address),
    }
}

fn is_private_provenance_ipv4(address: Ipv4Addr) -> bool {
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_unspecified()
}

fn is_private_provenance_ipv6(address: Ipv6Addr) -> bool {
    let first_segment = address.segments()[0];
    let is_unique_local = (first_segment & 0xfe00) == 0xfc00;
    let is_unicast_link_local = (first_segment & 0xffc0) == 0xfe80;
    address.is_loopback()
        || address.is_unspecified()
        || is_unique_local
        || is_unicast_link_local
        || address
            .to_ipv4_mapped()
            .is_some_and(is_private_provenance_ipv4)
}

fn validate_auth(auth: &AuthProof) -> Result<(), ValidationError> {
    match auth {
        AuthProof::LocalSecret { secret } if secret.is_empty() => {
            Err(ValidationError::AuthRequired)
        }
        AuthProof::LocalSecret { .. } => Ok(()),
    }
}

fn validate_control_command(command: &ControlCommand) -> Result<(), ValidationError> {
    match command {
        ControlCommand::CaptureScreenshot => Ok(()),
        ControlCommand::ExecuteJavascript { code } => validate_text(code, MAX_EVENT_TEXT_BYTES),
    }
}

fn validate_screenshot(screenshot: &ScreenshotPayload) -> Result<(), ValidationError> {
    validate_screenshot_bounds(
        screenshot.width,
        screenshot.height,
        screenshot.data_base64.len(),
    )
}

fn validate_screenshot_bounds(
    width: u32,
    height: u32,
    bytes: usize,
) -> Result<(), ValidationError> {
    if width > MAX_SCREENSHOT_WIDTH || height > MAX_SCREENSHOT_HEIGHT {
        return Err(ValidationError::InvalidDimensions);
    }
    if bytes > MAX_SCREENSHOT_BYTES {
        return Err(ValidationError::PayloadTooLarge {
            limit: MAX_SCREENSHOT_BYTES,
            actual: bytes,
        });
    }
    Ok(())
}

fn validate_timeout(timeout_ms: u64) -> Result<(), ValidationError> {
    if timeout_ms > MAX_CONTROL_TIMEOUT_MS {
        return Err(ValidationError::TimeoutTooLarge {
            limit_ms: MAX_CONTROL_TIMEOUT_MS,
            actual_ms: timeout_ms,
        });
    }
    Ok(())
}

fn validate_summary(summary: &str) -> Result<(), ValidationError> {
    validate_text(summary, MAX_MODEL_VISIBLE_SUMMARY_BYTES)
}

fn validate_text(text: &str, limit: usize) -> Result<(), ValidationError> {
    let actual = text.len();
    if actual > limit {
        return Err(ValidationError::PayloadTooLarge { limit, actual });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    AuthRequired,
    UnsupportedProtocolVersion,
    CapabilityDenied,
    InvalidEndpoint,
    InvalidDimensions,
    InvalidProvenance,
    PayloadTooLarge { limit: usize, actual: usize },
    TimeoutTooLarge { limit_ms: u64, actual_ms: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_match_public_caps() {
        assert_eq!(
            BridgeLimits::default(),
            BridgeLimits {
                max_message_bytes: MAX_MESSAGE_BYTES,
                max_retained_events: MAX_RETAINED_EVENTS,
                max_event_text_bytes: MAX_EVENT_TEXT_BYTES,
                max_screenshot_bytes: MAX_SCREENSHOT_BYTES,
                max_screenshot_width: MAX_SCREENSHOT_WIDTH,
                max_screenshot_height: MAX_SCREENSHOT_HEIGHT,
                max_control_timeout_ms: MAX_CONTROL_TIMEOUT_MS,
                max_model_visible_summary_bytes: MAX_MODEL_VISIBLE_SUMMARY_BYTES,
            }
        );
    }

    #[test]
    fn wire_names_are_camel_case_and_bounded_to_mvp_families() {
        let payload = BridgePayload::ControlRequest(ControlRequestMessage {
            request_id: "request-1".to_string(),
            requester_client_id: "requester-1".to_string(),
            target_client_id: "client-1".to_string(),
            command: ControlCommand::CaptureScreenshot,
            timeout_ms: 100,
        });

        let json = serde_json::to_string(&payload).unwrap();
        assert_eq!(message_family(&payload), "controlRequest");
        assert!(json.contains("controlRequest"));
        assert!(json.contains("captureScreenshot"));
    }

    #[test]
    fn hello_response_is_a_wire_payload_family() {
        let payload = BridgePayload::HelloResponse(HelloResponseMessage {
            accepted: true,
            protocol_version: PROTOCOL_VERSION.to_string(),
            granted_capabilities: CapabilitySet {
                subscribe_events: true,
                ..CapabilitySet::default()
            },
            client_session_token: Some("session-1".to_string()),
            limits: BridgeLimits::default(),
            error: None,
        });

        let json = serde_json::to_string(&payload).unwrap();
        assert_eq!(message_family(&payload), "helloResponse");
        assert!(json.contains("helloResponse"));
        assert_eq!(validate_payload(&payload), Ok(()));
    }

    #[test]
    fn descriptor_is_home_relative_and_does_not_reference_repo_scratch() {
        assert_eq!(DESCRIPTOR_RELATIVE_PATH, "code-bridge/descriptor.json");

        let descriptor = BridgeDescriptor {
            protocol_version: PROTOCOL_VERSION.to_string(),
            endpoint: BridgeEndpoint::LoopbackHttp {
                url: "http://127.0.0.1:12345".to_string(),
            },
            auth_secret: "secret".to_string(),
            pid: Some(42),
        };
        let json = serde_json::to_string(&descriptor).unwrap();

        assert!(json.contains("loopbackHttp"));
        assert!(!json.contains(".code"));
        assert!(!json.contains(".codex"));
        assert_eq!(validate_descriptor(&descriptor), Ok(()));
    }

    #[test]
    fn descriptor_rejects_non_loopback_or_incomplete_endpoints() {
        let descriptor = BridgeDescriptor {
            protocol_version: PROTOCOL_VERSION.to_string(),
            endpoint: BridgeEndpoint::LoopbackHttp {
                url: "http://example.com:12345".to_string(),
            },
            auth_secret: "secret".to_string(),
            pid: None,
        };
        assert_eq!(
            validate_descriptor(&descriptor),
            Err(ValidationError::InvalidEndpoint)
        );

        let descriptor = BridgeDescriptor {
            endpoint: BridgeEndpoint::LoopbackHttp {
                url: "http://127.0.0.1".to_string(),
            },
            ..descriptor
        };
        assert_eq!(
            validate_descriptor(&descriptor),
            Err(ValidationError::InvalidEndpoint)
        );

        let descriptor = BridgeDescriptor {
            endpoint: BridgeEndpoint::LoopbackHttp {
                url: "http://127.0.0.1:12345".to_string(),
            },
            auth_secret: String::new(),
            ..descriptor
        };
        assert_eq!(
            validate_descriptor(&descriptor),
            Err(ValidationError::AuthRequired)
        );
    }

    #[test]
    fn validates_protocol_version_and_message_size() {
        let envelope = BridgeEnvelope {
            protocol_version: "other".to_string(),
            message_id: "message-1".to_string(),
            timestamp_unix_ms: 1,
            payload: BridgePayload::Ack(AckMessage {
                message_id: "message-0".to_string(),
            }),
        };

        assert_eq!(
            validate_envelope(&envelope, 1),
            Err(ValidationError::UnsupportedProtocolVersion)
        );

        let envelope = BridgeEnvelope {
            protocol_version: PROTOCOL_VERSION.to_string(),
            ..envelope
        };
        assert_eq!(
            validate_envelope(&envelope, MAX_MESSAGE_BYTES + 1),
            Err(ValidationError::PayloadTooLarge {
                limit: MAX_MESSAGE_BYTES,
                actual: MAX_MESSAGE_BYTES + 1,
            })
        );
    }

    #[test]
    fn screenshot_response_uses_screenshot_specific_message_cap() {
        let payload = BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
            request_id: "request-1".to_string(),
            responding_client_id: "client-1".to_string(),
            status: ControlStatus::Ok,
            screenshot: Some(ScreenshotPayload {
                width: 100,
                height: 100,
                media_type: ScreenshotMediaType::Png,
                data_base64: "x".repeat(MAX_MESSAGE_BYTES + 1),
            }),
            error: None,
        });
        let envelope = BridgeEnvelope {
            protocol_version: PROTOCOL_VERSION.to_string(),
            message_id: "message-1".to_string(),
            timestamp_unix_ms: 1,
            payload,
        };

        assert_eq!(validate_envelope(&envelope, MAX_MESSAGE_BYTES + 1), Ok(()));
        assert_eq!(
            validate_envelope(&envelope, MAX_SCREENSHOT_MESSAGE_BYTES + 1),
            Err(ValidationError::PayloadTooLarge {
                limit: MAX_SCREENSHOT_MESSAGE_BYTES,
                actual: MAX_SCREENSHOT_MESSAGE_BYTES + 1,
            })
        );
    }

    #[test]
    fn producer_role_requires_producer_capability() {
        let hello = BridgePayload::Hello(HelloMessage {
            client_id: "client-1".to_string(),
            role: ClientRole::Producer,
            auth: AuthProof::LocalSecret {
                secret: "secret".to_string(),
            },
            requested_capabilities: CapabilitySet {
                subscribe_events: true,
                ..CapabilitySet::default()
            },
            metadata: ClientMetadata::default(),
        });

        assert_eq!(
            validate_payload(&hello),
            Err(ValidationError::CapabilityDenied)
        );
    }

    #[test]
    fn hello_rejects_cross_role_capabilities() {
        let hello = BridgePayload::Hello(HelloMessage {
            client_id: "client-1".to_string(),
            role: ClientRole::Producer,
            auth: AuthProof::LocalSecret {
                secret: "secret".to_string(),
            },
            requested_capabilities: CapabilitySet {
                publish_events: true,
                subscribe_events: true,
                ..CapabilitySet::default()
            },
            metadata: ClientMetadata::default(),
        });

        assert_eq!(
            validate_payload(&hello),
            Err(ValidationError::CapabilityDenied)
        );
    }

    #[test]
    fn hello_requires_non_empty_local_secret() {
        let hello = BridgePayload::Hello(HelloMessage {
            client_id: "client-1".to_string(),
            role: ClientRole::Producer,
            auth: AuthProof::LocalSecret {
                secret: String::new(),
            },
            requested_capabilities: CapabilitySet {
                publish_events: true,
                ..CapabilitySet::default()
            },
            metadata: ClientMetadata::default(),
        });

        assert_eq!(validate_payload(&hello), Err(ValidationError::AuthRequired));
    }

    #[test]
    fn hello_accepts_bounded_generic_provenance() {
        let hello = BridgePayload::Hello(HelloMessage {
            client_id: "client-1".to_string(),
            role: ClientRole::Producer,
            auth: AuthProof::LocalSecret {
                secret: "secret".to_string(),
            },
            requested_capabilities: CapabilitySet {
                publish_events: true,
                ..CapabilitySet::default()
            },
            metadata: ClientMetadata {
                source_kind: SourceKind::Cli,
                label: Some("launchplane worker".to_string()),
                provenance: Some(ProvenanceMetadata {
                    repository_url: Some("https://github.com/cbusillo/codex-lab".to_string()),
                    issue_or_pr_url: Some(
                        "https://github.com/cbusillo/codex-lab/issues/49".to_string(),
                    ),
                    request_id: Some("lp-request-1".to_string()),
                    trace_id: Some("trace-1".to_string()),
                    environment_label: Some("local-dev".to_string()),
                }),
            },
        });

        assert_eq!(validate_payload(&hello), Ok(()));
    }

    #[test]
    fn hello_rejects_unbounded_or_sensitive_provenance_shapes() {
        let mut hello = HelloMessage {
            client_id: "client-1".to_string(),
            role: ClientRole::Producer,
            auth: AuthProof::LocalSecret {
                secret: "secret".to_string(),
            },
            requested_capabilities: CapabilitySet {
                publish_events: true,
                ..CapabilitySet::default()
            },
            metadata: ClientMetadata {
                source_kind: SourceKind::Cli,
                label: Some("x".repeat(MAX_CLIENT_LABEL_BYTES + 1)),
                provenance: None,
            },
        };

        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::PayloadTooLarge {
                limit: MAX_CLIENT_LABEL_BYTES,
                actual: MAX_CLIENT_LABEL_BYTES + 1,
            })
        );

        hello.metadata.label = None;
        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: Some("http://github.com/cbusillo/codex-lab".to_string()),
            issue_or_pr_url: None,
            request_id: None,
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::InvalidProvenance)
        );

        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: Some("https://token@example.com/cbusillo/codex-lab".to_string()),
            issue_or_pr_url: None,
            request_id: None,
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::InvalidProvenance)
        );

        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: Some("https://github.com/cbusillo/codex-lab?token=secret".to_string()),
            issue_or_pr_url: None,
            request_id: None,
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::InvalidProvenance)
        );

        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: Some("https://127.0.0.1/cbusillo/codex-lab".to_string()),
            issue_or_pr_url: None,
            request_id: None,
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::InvalidProvenance)
        );

        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: Some("https://[fc00::1]/cbusillo/codex-lab".to_string()),
            issue_or_pr_url: None,
            request_id: None,
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::InvalidProvenance)
        );

        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: Some("https://[fe80::1]/cbusillo/codex-lab".to_string()),
            issue_or_pr_url: None,
            request_id: None,
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::InvalidProvenance)
        );

        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: Some("https://[::ffff:192.168.1.1]/cbusillo/codex-lab".to_string()),
            issue_or_pr_url: None,
            request_id: None,
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::InvalidProvenance)
        );

        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: None,
            issue_or_pr_url: Some("file:///Users/cbusillo/private".to_string()),
            request_id: None,
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::InvalidProvenance)
        );

        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: None,
            issue_or_pr_url: None,
            request_id: Some("not a token".to_string()),
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello.clone())),
            Err(ValidationError::InvalidProvenance)
        );

        hello.metadata.provenance = Some(ProvenanceMetadata {
            repository_url: None,
            issue_or_pr_url: None,
            request_id: Some("x".repeat(MAX_PROVENANCE_ID_BYTES + 1)),
            trace_id: None,
            environment_label: None,
        });
        assert_eq!(
            validate_payload(&BridgePayload::Hello(hello)),
            Err(ValidationError::PayloadTooLarge {
                limit: MAX_PROVENANCE_ID_BYTES,
                actual: MAX_PROVENANCE_ID_BYTES + 1,
            })
        );
    }

    #[test]
    fn screenshot_and_javascript_require_explicit_capabilities() {
        let event = BridgeEvent::Screenshot(ScreenshotEvent {
            screenshot_id: "screenshot-1".to_string(),
            width: 100,
            height: 100,
            media_type: ScreenshotMediaType::Png,
            bytes: 10,
        });
        let publish_only = CapabilitySet {
            publish_events: true,
            ..CapabilitySet::default()
        };
        assert_eq!(
            validate_event_capabilities(&publish_only, &event),
            Err(ValidationError::CapabilityDenied)
        );

        let screenshot_provider = CapabilitySet {
            publish_events: true,
            provide_screenshot: true,
            ..CapabilitySet::default()
        };
        assert_eq!(
            validate_event_capabilities(&screenshot_provider, &event),
            Ok(())
        );

        let command = ControlCommand::ExecuteJavascript {
            code: "window.location.href".to_string(),
        };
        let screenshot_requester = CapabilitySet {
            request_screenshot: true,
            ..CapabilitySet::default()
        };
        assert_eq!(
            validate_control_capabilities(&screenshot_requester, &command),
            Err(ValidationError::CapabilityDenied)
        );

        let control_requester = CapabilitySet {
            request_control: true,
            ..CapabilitySet::default()
        };
        assert_eq!(
            validate_control_capabilities(&control_requester, &command),
            Ok(())
        );
    }

    #[test]
    fn text_and_summary_payloads_are_capped() {
        let oversized_text = "x".repeat(MAX_EVENT_TEXT_BYTES + 1);
        let payload = BridgePayload::Event(EventPublishMessage {
            client_id: "client-1".to_string(),
            event_id: "event-1".to_string(),
            event: BridgeEvent::Console(ConsoleEvent {
                level: ConsoleLevel::Info,
                text: oversized_text,
            }),
        });

        assert_eq!(
            validate_payload(&payload),
            Err(ValidationError::PayloadTooLarge {
                limit: MAX_EVENT_TEXT_BYTES,
                actual: MAX_EVENT_TEXT_BYTES + 1,
            })
        );

        let oversized_summary = "x".repeat(MAX_MODEL_VISIBLE_SUMMARY_BYTES + 1);
        let payload = BridgePayload::ControlResponse(ControlResponseMessage {
            request_id: "request-1".to_string(),
            responding_client_id: "client-1".to_string(),
            status: ControlStatus::Ok,
            summary: oversized_summary,
            error: None,
        });

        assert_eq!(
            validate_payload(&payload),
            Err(ValidationError::PayloadTooLarge {
                limit: MAX_MODEL_VISIBLE_SUMMARY_BYTES,
                actual: MAX_MODEL_VISIBLE_SUMMARY_BYTES + 1,
            })
        );
    }

    #[test]
    fn screenshot_and_timeout_bounds_are_enforced() {
        let payload = BridgePayload::ScreenshotResponse(ScreenshotResponseMessage {
            request_id: "request-1".to_string(),
            responding_client_id: "client-1".to_string(),
            status: ControlStatus::Ok,
            screenshot: Some(ScreenshotPayload {
                width: MAX_SCREENSHOT_WIDTH + 1,
                height: 100,
                media_type: ScreenshotMediaType::Png,
                data_base64: String::new(),
            }),
            error: None,
        });

        assert_eq!(
            validate_payload(&payload),
            Err(ValidationError::InvalidDimensions)
        );

        let payload = BridgePayload::ScreenshotRequest(ScreenshotRequestMessage {
            request_id: "request-1".to_string(),
            requester_client_id: "requester-1".to_string(),
            target_client_id: "client-1".to_string(),
            timeout_ms: MAX_CONTROL_TIMEOUT_MS + 1,
        });

        assert_eq!(
            validate_payload(&payload),
            Err(ValidationError::TimeoutTooLarge {
                limit_ms: MAX_CONTROL_TIMEOUT_MS,
                actual_ms: MAX_CONTROL_TIMEOUT_MS + 1,
            })
        );
    }
}
