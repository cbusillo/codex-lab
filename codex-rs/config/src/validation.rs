use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

pub const DEFAULT_PROJECT_VALIDATION_TIMEOUT_MS: u64 = 60_000;
pub const MAX_PROJECT_VALIDATION_TIMEOUT_MS: u64 = 30 * 60 * 1_000;
pub const DEFAULT_VALIDATION_PROVIDER_TIMEOUT_MS: u64 = 6_000;
pub const MAX_VALIDATION_PROVIDER_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_CARGO_VALIDATION_PROVIDER_TIMEOUT_MS: u64 = MAX_VALIDATION_PROVIDER_TIMEOUT_MS;

const fn default_true() -> bool {
    true
}

const fn default_project_validation_timeout_ms() -> u64 {
    DEFAULT_PROJECT_VALIDATION_TIMEOUT_MS
}

fn default_shellcheck_command() -> Vec<String> {
    vec!["shellcheck".to_string()]
}

fn default_cargo_command() -> Vec<String> {
    vec!["cargo".to_string()]
}

const fn default_validation_provider_timeout_ms() -> u64 {
    DEFAULT_VALIDATION_PROVIDER_TIMEOUT_MS
}

const fn default_cargo_validation_provider_timeout_ms() -> u64 {
    DEFAULT_CARGO_VALIDATION_PROVIDER_TIMEOUT_MS
}

/// Patch-local validation settings.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ValidationConfig {
    #[serde(default)]
    pub groups: ValidationGroups,
    #[serde(default)]
    pub providers: ValidationProviders,
    #[serde(default)]
    pub project_command: Option<ProjectValidationCommand>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ValidationProviders {
    #[serde(default)]
    pub cargo: CargoValidationProviderConfig,
    #[serde(default)]
    pub shellcheck: ShellcheckValidationProviderConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CargoValidationProviderConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_cargo_command")]
    pub command: Vec<String>,
    #[serde(default = "default_cargo_validation_provider_timeout_ms")]
    #[schemars(range(min = 1, max = 30000))]
    pub timeout_ms: u64,
}

impl Default for CargoValidationProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: default_cargo_command(),
            timeout_ms: DEFAULT_CARGO_VALIDATION_PROVIDER_TIMEOUT_MS,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ShellcheckValidationProviderConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_shellcheck_command")]
    pub command: Vec<String>,
    #[serde(default = "default_validation_provider_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for ShellcheckValidationProviderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: default_shellcheck_command(),
            timeout_ms: DEFAULT_VALIDATION_PROVIDER_TIMEOUT_MS,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ProjectValidationCommand {
    #[serde(default)]
    pub command: Vec<String>,
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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
#[derive(Default)]
pub struct ValidationGroups {
    #[serde(default = "default_true")]
    pub functional: bool,
    #[serde(default)]
    pub stylistic: bool,
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
