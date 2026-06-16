use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeBridgeStatusReadResponse {
    pub status: CodeBridgeAvailability,
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
