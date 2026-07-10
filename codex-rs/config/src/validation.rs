use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

const fn default_true() -> bool {
    true
}

/// Patch-local validation settings.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ValidationConfig {
    /// Validation categories enabled for patch-local checks.
    #[serde(default)]
    pub groups: ValidationGroups,
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
