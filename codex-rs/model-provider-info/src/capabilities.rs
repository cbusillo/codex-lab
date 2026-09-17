use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Tool-surface capabilities a provider declares for the models it serves.
///
/// Every field defaults to `true`, so providers that omit the `capabilities`
/// table keep the current tool surface. Local OpenAI-compatible servers that
/// only accept top-level `function` tools should set the unsupported
/// capabilities to `false`; Codex then emits a flat tool surface for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelProviderCapabilities {
    /// Whether the provider accepts the Responses API `namespace` tool type.
    #[serde(default = "default_true")]
    pub namespace_tools: bool,
    /// Whether the provider accepts freeform `custom` tools such as `apply_patch`.
    #[serde(default = "default_true")]
    pub custom_tools: bool,
    /// Whether the provider accepts the hosted `web_search` tool.
    #[serde(default = "default_true")]
    pub web_search: bool,
}

impl Default for ModelProviderCapabilities {
    fn default() -> Self {
        Self {
            namespace_tools: true,
            custom_tools: true,
            web_search: true,
        }
    }
}

fn default_true() -> bool {
    true
}
