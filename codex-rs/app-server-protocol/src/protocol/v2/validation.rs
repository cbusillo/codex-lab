use super::shared::v2_enum_from_core;
use codex_protocol::protocol::ProjectValidationSkipReason as CoreProjectValidationSkipReason;
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
        Cancelled,
        Skipped,
    }
);

v2_enum_from_core!(
    pub enum ProjectValidationSkipReason from CoreProjectValidationSkipReason {
        ValidationDisabled,
        NoChangedFiles,
        NoApplicableProvider,
        NonRootAgent,
        UnchangedFingerprint,
        UnsupportedEnvironment,
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
    pub item_id: Option<String>,
    pub command: Vec<String>,
    pub command_truncated: bool,
    pub cwd: Option<AbsolutePathBuf>,
    pub status: ProjectValidationStatus,
    pub skip_reason: Option<ProjectValidationSkipReason>,
    pub changed_file_count: Option<u32>,
    pub exit_code: Option<i32>,
    pub output: String,
    pub output_truncated: bool,
    pub duration_ms: u64,
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
