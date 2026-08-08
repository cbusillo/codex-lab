use super::*;
use color_eyre::eyre::WrapErr;
use pretty_assertions::assert_eq;
use std::path::Path;

#[test]
fn app_scoped_key_path_quotes_dotted_app_ids() {
    assert_eq!(
        app_scoped_key_path("plugin.linear", "enabled"),
        "apps.\"plugin.linear\".enabled"
    );
}

#[test]
fn agent_selector_edit_quotes_dotted_selector_names() {
    assert_eq!(
        agent_selector_enabled_edit("antigravity-gemini-3.6-flash", /*enabled*/ false),
        ConfigEdit {
            key_path: "agents.selectors.\"antigravity-gemini-3.6-flash\".enabled".to_string(),
            value: serde_json::json!(false),
            merge_strategy: MergeStrategy::Replace,
        }
    );
}

#[test]
fn agent_selector_default_edits_use_typed_model_and_effort_fields() {
    assert_eq!(
        agent_selector_model_edit("antigravity", Some("gemini-3-pro")),
        ConfigEdit {
            key_path: "agents.selectors.\"antigravity\".model".to_string(),
            value: serde_json::json!("gemini-3-pro"),
            merge_strategy: MergeStrategy::Replace,
        }
    );
    assert_eq!(
        agent_selector_effort_edit("antigravity", /*effort*/ None),
        ConfigEdit {
            key_path: "agents.selectors.\"antigravity\".effort".to_string(),
            value: serde_json::Value::Null,
            merge_strategy: MergeStrategy::Replace,
        }
    );
}

#[test]
fn trusted_project_edit_targets_project_trust_level() {
    assert_eq!(
        trusted_project_edit(Path::new("/workspace/team.project")),
        ConfigEdit {
            key_path: "projects.\"/workspace/team.project\".trust_level".to_string(),
            value: serde_json::json!("trusted"),
            merge_strategy: MergeStrategy::Replace,
        }
    );
}

#[test]
fn format_config_error_preserves_server_validation_message() {
    let err = Err::<(), _>(color_eyre::eyre::eyre!(
        "config/batchWrite failed: Invalid configuration: features.fast_mode=true violates \
         managed requirements; allowed set [fast_mode=false]"
    ))
    .wrap_err("config/batchWrite failed in TUI")
    .unwrap_err();

    assert_eq!(
        format_config_error(&err),
        "config/batchWrite failed in TUI: config/batchWrite failed: Invalid configuration: \
         features.fast_mode=true violates managed requirements; allowed set [fast_mode=false]"
    );
}
