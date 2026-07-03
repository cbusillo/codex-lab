use super::*;
use codex_app_server_protocol::ConfigEdit;
use pretty_assertions::assert_eq;

#[test]
fn hook_trust_config_write_params_upserts_hooks_state() {
    let params = hook_trust_config_write_params(vec![
        HookTrustUpdate {
            key: "user-hook".to_string(),
            current_hash: "sha256-user".to_string(),
        },
        HookTrustUpdate {
            key: "plugin-hook".to_string(),
            current_hash: "sha256-plugin".to_string(),
        },
    ]);

    assert_eq!(
        params,
        ConfigBatchWriteParams {
            edits: vec![ConfigEdit {
                key_path: "hooks.state".to_string(),
                value: serde_json::json!({
                    "user-hook": { "trusted_hash": "sha256-user" },
                    "plugin-hook": { "trusted_hash": "sha256-plugin" },
                }),
                merge_strategy: MergeStrategy::Upsert,
            }],
            file_path: None,
            expected_version: None,
            reload_user_config: true,
        }
    );
}
