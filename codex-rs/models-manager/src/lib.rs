pub mod cache;
pub mod collaboration_mode_presets;
pub(crate) mod config;
pub mod manager;
pub mod model_info;
pub mod model_presets;
pub mod test_support;

pub use codex_protocol::auth::AuthMode;
pub use config::ModelsManagerConfig;

/// Load the bundled model catalog shipped with `codex-models-manager`.
pub fn bundled_models_response()
-> std::result::Result<codex_protocol::openai_models::ModelsResponse, serde_json::Error> {
    let mut response: serde_json::Value = serde_json::from_str(include_str!("../models.json"))?;
    // The bundled Astra entry contains capability data, not a second copy of the
    // upstream prompt stack. Reuse the existing local fallback until discovery
    // supplies account-specific instructions from the remote catalog.
    if let Some(models) = response["models"].as_array_mut() {
        for model in models {
            if model["slug"] == "gpt-6-astra" && model["model_messages"].is_null() {
                model["base_instructions"] = model_info::BASE_INSTRUCTIONS.into();
            }
        }
    }
    serde_json::from_value(response)
}

/// Whole compatibility version used for model discovery and cache eligibility.
pub fn client_version_to_whole() -> String {
    codex_version::models_discovery_version()
        .split(['-', '+'])
        .next()
        .unwrap_or_default()
        .to_string()
}
