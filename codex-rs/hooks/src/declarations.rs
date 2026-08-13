use std::collections::HashSet;

use codex_plugin::PluginHookSource;
use codex_protocol::protocol::HookEventName;

/// Minimal declaration metadata for one bundled plugin hook handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginHookDeclaration {
    pub key: String,
    pub event_name: HookEventName,
}

/// Return the hook handlers declared by plugin bundles without projecting live runtime state.
pub fn plugin_hook_declarations(hook_sources: &[PluginHookSource]) -> Vec<PluginHookDeclaration> {
    let mut declarations = Vec::new();

    for source in hook_sources {
        let key_source = plugin_hook_key_source(
            source.plugin_id.as_key().as_str(),
            source.source_relative_path.as_str(),
        );
        for (event_name, groups) in source.hooks.clone().into_matcher_groups() {
            let mut seen_keys = HashSet::new();
            for (group_index, group) in groups.iter().enumerate() {
                for (handler_index, handler) in group.hooks.iter().enumerate() {
                    let requested_key = crate::hook_key(
                        &key_source,
                        event_name,
                        group_index,
                        handler_index,
                        hook_handler_id(handler),
                    );
                    let key = if seen_keys.insert(requested_key.clone()) {
                        requested_key
                    } else {
                        crate::hook_key(
                            &key_source,
                            event_name,
                            group_index,
                            handler_index,
                            /*id*/ None,
                        )
                    };
                    declarations.push(PluginHookDeclaration { key, event_name });
                }
            }
        }
    }

    declarations
}

pub(crate) fn plugin_hook_key_source(plugin_id: &str, source_relative_path: &str) -> String {
    format!("{plugin_id}:{source_relative_path}")
}

/// The declared hook id, but only for handlers that discovery will actually
/// register. Skipped handlers must not claim an id-based key that a later
/// runnable handler with the same id would then be denied.
pub(crate) fn hook_handler_id(handler: &codex_config::HookHandlerConfig) -> Option<&str> {
    match handler {
        codex_config::HookHandlerConfig::Command { id, command, .. } => {
            id.as_deref().filter(|_| !command.trim().is_empty())
        }
        codex_config::HookHandlerConfig::McpTool { .. }
        | codex_config::HookHandlerConfig::Prompt {}
        | codex_config::HookHandlerConfig::Agent {} => None,
    }
}

#[cfg(test)]
mod tests {
    use codex_config::HookEventsToml;
    use codex_config::HookHandlerConfig;
    use codex_config::MatcherGroup;
    use codex_plugin::PluginId;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn lists_declared_plugin_handlers_with_persisted_hook_keys() {
        let plugin_root = test_path_buf("/tmp/plugin").abs();
        let source_path = plugin_root.join("hooks/hooks.json");
        let declarations = plugin_hook_declarations(&[PluginHookSource {
            plugin_id: PluginId::parse("demo@test").expect("plugin id"),
            plugin_root: plugin_root.clone(),
            plugin_data_root: plugin_root.join("data"),
            source_path,
            source_relative_path: "hooks/hooks.json".to_string(),
            hooks: HookEventsToml {
                pre_tool_use: vec![MatcherGroup {
                    matcher: None,
                    hooks: vec![
                        HookHandlerConfig::Prompt {},
                        HookHandlerConfig::Command {
                            id: None,
                            command: "echo hi".to_string(),
                            command_windows: None,
                            timeout_sec: None,
                            r#async: false,
                            status_message: None,
                            additional_context_limit: None,
                        },
                    ],
                }],
                session_start: vec![MatcherGroup {
                    matcher: None,
                    hooks: vec![HookHandlerConfig::Agent {}],
                }],
                ..Default::default()
            },
        }]);

        assert_eq!(
            declarations,
            vec![
                PluginHookDeclaration {
                    key: "demo@test:hooks/hooks.json:pre_tool_use:0:0".to_string(),
                    event_name: HookEventName::PreToolUse,
                },
                PluginHookDeclaration {
                    key: "demo@test:hooks/hooks.json:pre_tool_use:0:1".to_string(),
                    event_name: HookEventName::PreToolUse,
                },
                PluginHookDeclaration {
                    key: "demo@test:hooks/hooks.json:session_start:0:0".to_string(),
                    event_name: HookEventName::SessionStart,
                },
            ]
        );
    }
}
