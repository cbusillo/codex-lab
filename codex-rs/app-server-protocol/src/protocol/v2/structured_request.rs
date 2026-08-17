use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StructuredRequestStartParams {
    pub request_id: String,
    pub model: String,
    pub developer_instructions: String,
    pub user_input: String,
    #[ts(type = "unknown")]
    pub output_schema: Value,
    #[ts(optional = nullable)]
    pub max_output_tokens: Option<u64>,
    #[ts(optional = nullable)]
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StructuredRequestStartResponse {
    pub request_id: String,
    pub model: String,
    pub response_id: String,
    #[ts(type = "unknown")]
    pub output: Value,
    pub usage: Option<StructuredRequestTokenUsage>,
    pub duration_ms: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StructuredRequestTokenUsage {
    #[ts(type = "number")]
    pub input_tokens: i64,
    #[ts(type = "number")]
    pub cached_input_tokens: i64,
    #[ts(type = "number")]
    pub cache_write_input_tokens: i64,
    #[ts(type = "number")]
    pub output_tokens: i64,
    #[ts(type = "number")]
    pub reasoning_output_tokens: i64,
    #[ts(type = "number")]
    pub total_tokens: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StructuredRequestCancelParams {
    pub request_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StructuredRequestCancelResponse {
    pub cancelled: bool,
}
