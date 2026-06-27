use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeStatusReadResponse {
    pub status: CodeBridgeAvailability,
    pub control_available: bool,
    pub service: Option<CodeBridgeServiceStatus>,
    pub unavailable_reason: Option<CodeBridgeUnavailableReason>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeBridgeAvailability {
    Available,
    Unavailable,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeBridgeUnavailableReason {
    DescriptorMissing,
    DescriptorInvalid,
    UnsupportedEndpoint,
    ServiceUnreachable,
    StatusInvalid,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeServiceStatus {
    pub protocol_version: String,
    pub connected_producer_count: usize,
    pub connected_subscriber_count: usize,
    pub uptime_ms: u64,
    pub last_event_time_unix_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeSubscribeParams {
    pub filter: CodeBridgeSubscriptionFilter,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeSubscribeResponse {
    pub status: CodeBridgeRequestStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeSubscriptionFilter {
    pub levels: Vec<CodeBridgeConsoleLevel>,
    pub event_kinds: Vec<CodeBridgeEventKind>,
    pub client_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeBridgeConsoleLevel {
    Trace,
    Info,
    Warn,
    Error,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeBridgeEventKind {
    Console,
    Error,
    Pageview,
    Screenshot,
    ControlResult,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeScreenshotParams {
    pub target_client_id: String,
    #[ts(optional = nullable)]
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeScreenshotResponse {
    pub request_id: String,
    pub status: CodeBridgeControlStatus,
    pub screenshot: Option<CodeBridgeScreenshotPayload>,
    pub error: Option<CodeBridgeError>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeJavascriptParams {
    pub target_client_id: String,
    pub code: String,
    #[ts(optional = nullable)]
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeJavascriptResponse {
    pub request_id: String,
    pub status: CodeBridgeControlStatus,
    pub summary: String,
    pub result: Option<Value>,
    pub error: Option<CodeBridgeError>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeBridgeRequestStatus {
    Accepted,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeError {
    pub code: CodeBridgeErrorCode,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeBridgeErrorCode {
    AuthRequired,
    AuthRejected,
    CapabilityDenied,
    InvalidPayload,
    PayloadTooLarge,
    Timeout,
    UnsupportedProtocolVersion,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeBridgeControlStatus {
    Ok,
    Failed,
    TimedOut,
    Denied,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeBridgeScreenshotMediaType {
    Png,
    Jpeg,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeScreenshotPayload {
    pub width: u32,
    pub height: u32,
    pub media_type: CodeBridgeScreenshotMediaType,
    pub data_base64: String,
}
