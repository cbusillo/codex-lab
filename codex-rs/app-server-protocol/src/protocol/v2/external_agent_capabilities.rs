use super::ConfigLayerMetadata;
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentCapabilitiesReadParams {
    #[ts(optional = nullable)]
    pub cwd: Option<String>,
    #[ts(optional = nullable)]
    pub families: Option<Vec<String>>,
    #[ts(optional = nullable)]
    pub refresh: Option<ExternalAgentCapabilitiesRefreshParams>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentCapabilitiesRefreshParams {
    pub refresh_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentCapabilitiesReadResponse {
    pub providers: Vec<ExternalAgentProviderCapabilities>,
    pub refresh: Option<ExternalAgentCapabilitiesRefreshHandle>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentCapabilitiesRefreshHandle {
    pub refresh_id: String,
    pub families: Vec<String>,
    pub deadline_ms: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentProviderCapabilities {
    pub family: String,
    pub command: String,
    pub install: ExternalAgentInstallState,
    pub auth: ExternalAgentAuthState,
    pub cli_version: Option<String>,
    pub supports_model_selection: bool,
    pub supports_effort_selection: bool,
    pub effort_levels: Vec<String>,
    pub source: ExternalAgentCapabilitySource,
    pub freshness: ExternalAgentCapabilityFreshness,
    pub observed_at: Option<i64>,
    pub failure: Option<ExternalAgentCapabilityFailure>,
    pub selectors: Vec<ExternalAgentSelectorState>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentSelectorState {
    pub selector: String,
    pub discovered: bool,
    pub model: Option<String>,
    pub explicit_only: bool,
    pub enabled: bool,
    pub enabled_origin: Option<ExternalAgentSettingOrigin>,
    pub default_model: Option<String>,
    pub default_model_origin: Option<ExternalAgentSettingOrigin>,
    pub default_effort: Option<String>,
    pub default_effort_origin: Option<ExternalAgentSettingOrigin>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentSettingOrigin {
    pub config_path: String,
    pub layer: ConfigLayerMetadata,
    pub read_only: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ExternalAgentInstallState {
    Installed,
    NotInstalled,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ExternalAgentAuthState {
    Authenticated,
    NotAuthenticated,
    NotProbed,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ExternalAgentCapabilitySource {
    StaticCatalog,
    LocalCli,
    NotProbed,
    ConservativeFallback,
    CacheMiss,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ExternalAgentCapabilityFreshness {
    Fresh,
    Cached,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentCapabilityFailure {
    pub kind: ExternalAgentCapabilityFailureKind,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ExternalAgentCapabilityFailureKind {
    CommandMissing,
    AuthenticationRequired,
    QuotaOrRateLimited,
    UnsupportedMode,
    TimedOut,
    MalformedOutput,
    EmptyOutput,
    LaunchFailed,
    ProviderFailed,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentCapabilitiesRefreshCancelParams {
    pub refresh_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentCapabilitiesRefreshCancelResponse {
    pub cancelled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ExternalAgentCapabilitiesUpdatedNotification {
    pub refresh_id: String,
    pub status: ExternalAgentCapabilitiesRefreshStatus,
    pub providers: Vec<ExternalAgentProviderCapabilities>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ExternalAgentCapabilitiesRefreshStatus {
    Completed,
    Cancelled,
    DeadlineExceeded,
}
