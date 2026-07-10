use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const DEFAULT_PROJECT_VALIDATION_TIMEOUT_MS: u64 = 60_000;
pub const MAX_PROJECT_VALIDATION_TIMEOUT_MS: u64 = 30 * 60 * 1_000;

const fn default_true() -> bool {
    true
}

const fn default_project_validation_timeout_ms() -> u64 {
    DEFAULT_PROJECT_VALIDATION_TIMEOUT_MS
}

/// Patch-local validation settings.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ValidationConfig {
    /// Validation categories enabled for patch-local checks.
    #[serde(default)]
    pub groups: ValidationGroups,

    /// Optional project command to run after a root turn finishes agent work.
    /// An actionable failure sends a bounded command/output fragment to the
    /// model, permits one correction cycle, and reruns the command once. The
    /// command should therefore tolerate up to two executions per root turn.
    /// Repository-local config cannot set this executable field.
    #[serde(default)]
    pub project_command: Option<ProjectValidationCommand>,
}

/// One project-defined command that runs automatically after agent work.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ProjectValidationCommand {
    /// Executable and arguments. Shell syntax is not interpreted.
    #[serde(default)]
    pub command: Vec<String>,

    /// Maximum execution time in milliseconds.
    #[serde(default = "default_project_validation_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for ProjectValidationCommand {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            timeout_ms: DEFAULT_PROJECT_VALIDATION_TIMEOUT_MS,
        }
    }
}

/// Category toggles for patch-local validation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
#[derive(Default)]
pub struct ValidationGroups {
    /// Structural and correctness checks.
    #[serde(default = "default_true")]
    pub functional: bool,

    /// Formatting and style checks. Reserved for a later runtime slice.
    #[serde(default)]
    pub stylistic: bool,
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
