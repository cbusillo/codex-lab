use super::shared::v2_enum_from_core;
use codex_protocol::protocol::ProjectValidationStatus as CoreProjectValidationStatus;
use codex_utils_absolute_path::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

v2_enum_from_core!(
    pub enum ProjectValidationStatus from CoreProjectValidationStatus {
        Passed,
        ActionableFailure,
        ConfigurationError,
        TimedOut,
        InfrastructureFailure,
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
/// One completed project-validation command execution. An actionable first execution may be
/// followed by one bounded correction cycle and a second completion notification.
pub struct ProjectValidationCompletedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<AbsolutePathBuf>,
    pub status: ProjectValidationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_code: Option<i32>,
    pub output: String,
    pub output_truncated: bool,
    pub duration_ms: u64,
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
