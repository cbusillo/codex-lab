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
    /// Validation categories enabled for patch-local checks.
    #[serde(default)]
    pub groups: ValidationGroups,

    /// Automatically selected executable providers. Provider argv is trusted
    /// executable configuration and cannot be supplied by repository-local
    /// config.
    #[serde(default)]
    pub providers: ValidationProviders,

    /// Optional project command to run after a root turn changes a Git worktree.
    /// Model tool activity, non-Git workspaces, and unreadable worktree state
    /// preserve the existing fail-open behavior and run the command. An
    /// actionable failure sends a bounded command/output fragment to the model,
    /// permits one correction cycle, and reruns the command once. The command
    /// should therefore tolerate up to two executions per root turn. Commands
    /// targeting the same Git repository are serialized across root sessions
    /// in one runtime; non-Git workspaces remain uncoordinated. A waiting turn
    /// can be cancelled and correction reruns reacquire the repository lease.
    /// Repository-local config cannot set this executable field.
    #[serde(default)]
    pub project_command: Option<ProjectValidationCommand>,
}

/// Automatically selected executable validation providers.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ValidationProviders {
    /// Cargo validation selected from changed Rust and Cargo workspace files.
    #[serde(default)]
    pub cargo: CargoValidationProviderConfig,

    /// Shell script validation selected from changed `.sh` and shebang files.
    #[serde(default)]
    pub shellcheck: ShellcheckValidationProviderConfig,
}

/// Trusted configuration for the built-in Cargo provider. Changed repository
/// Cargo config is parsed for errors, but Cargo runs from an isolated directory
/// so repository config cannot redirect validation through local executables.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct CargoValidationProviderConfig {
    /// Whether changed Rust and Cargo workspace files select this provider.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Trusted Cargo executable. The provider requires exactly one executable
    /// and appends the built-in `check` subcommand and fixed bounded arguments.
    /// Repository-local values are ignored with a warning.
    #[serde(default = "default_cargo_command")]
    pub command: Vec<String>,

    /// Maximum execution time in milliseconds.
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

/// Trusted configuration for the built-in ShellCheck provider.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ShellcheckValidationProviderConfig {
    /// Whether changed shell files select this provider.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Executable and optional trusted prefix arguments. Only user, system,
    /// managed, or runtime config may set this field; repository-local values
    /// are ignored with a warning. The runtime appends `-f gcc` and the selected
    /// changed-file paths without shell parsing.
    #[serde(default = "default_shellcheck_command")]
    pub command: Vec<String>,

    /// Maximum execution time in milliseconds.
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
