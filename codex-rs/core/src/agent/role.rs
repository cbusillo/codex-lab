//! Applies agent-role configuration layers on top of an existing session config.
//!
//! Roles are selected at spawn time and are loaded with the same config machinery as
//! `config.toml`. This module resolves built-in and user-defined role files, inserts the role as a
//! high-precedence layer, and preserves the caller's current model, reasoning effort, provider,
//! and service tier unless the role layer sets them. It does not decide when to spawn a sub-agent
//! or which role to use; the multi-agent tool handler owns that orchestration.

use crate::config::AgentRoleBackendConfig;
use crate::config::AgentRoleConfig;
use crate::config::Config;
use crate::config::ConfigOverrides;
use crate::config::ExternalCommandAgentBackendConfig;
use crate::config::ExternalCommandProtocol;
use crate::config::agent_roles::parse_agent_role_file_contents;
use crate::config::deserialize_config_toml_with_base;
use anyhow::anyhow;
use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::config_toml::ConfigToml;
use codex_config::loader::resolve_relative_paths_in_config_toml;
use codex_exec_server::LOCAL_FS;
use codex_features::Feature;
use codex_protocol::models::BaseInstructionsProvenance;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;
use toml::Value as TomlValue;

/// The role name used when a caller omits `agent_type`.
pub const DEFAULT_ROLE_NAME: &str = "default";
const AGENT_TYPE_UNAVAILABLE_ERROR: &str = "agent type is currently not available";

/// Applies a named role layer to `config` while preserving caller-owned provider settings.
///
/// The role layer is inserted at session-flag precedence so it can override persisted config, but
/// the caller's current `model_provider` and `service_tier` remain sticky runtime choices unless
/// the role explicitly sets the corresponding top-level config key. Rebuilding the config without
/// those overrides would make a spawned agent silently fall back to default settings.
pub(crate) async fn apply_role_to_config(
    config: &mut Config,
    role_name: Option<&str>,
) -> Result<(), String> {
    apply_role_to_config_with_developer_instructions(
        config,
        role_name,
        RoleDeveloperInstructions::UseConfigLayers,
    )
    .await
}

/// Applies a v2 role without losing developer instructions selected by its caller.
///
/// A role's own top-level developer instructions still take precedence. When its role file omits
/// that setting, rebuilding the config must not restore inherited instructions from older layers.
pub(crate) async fn apply_role_to_config_for_multi_agent_v2(
    config: &mut Config,
    role_name: Option<&str>,
) -> Result<(), String> {
    apply_role_to_config_with_developer_instructions(
        config,
        role_name,
        RoleDeveloperInstructions::PreserveCallerInstructions,
    )
    .await
}

#[derive(Clone, Copy)]
enum RoleDeveloperInstructions {
    UseConfigLayers,
    PreserveCallerInstructions,
}

async fn apply_role_to_config_with_developer_instructions(
    config: &mut Config,
    role_name: Option<&str>,
    developer_instructions: RoleDeveloperInstructions,
) -> Result<(), String> {
    let role_name = role_name.unwrap_or(DEFAULT_ROLE_NAME);

    let role = resolve_role_config_owned(config, role_name)
        .ok_or_else(|| format!("unknown agent_type '{role_name}'"))?;

    apply_role_to_config_inner(config, role_name, &role, developer_instructions)
        .await
        .map_err(|err| {
            tracing::warn!("failed to apply role to config: {err}");
            AGENT_TYPE_UNAVAILABLE_ERROR.to_string()
        })
}

async fn apply_role_to_config_inner(
    config: &mut Config,
    role_name: &str,
    role: &AgentRoleConfig,
    developer_instructions: RoleDeveloperInstructions,
) -> anyhow::Result<()> {
    let is_built_in = !config.agent_roles.contains_key(role_name);
    let Some(config_file) = role.config_file.as_ref() else {
        return Ok(());
    };
    let role_layer_toml = load_role_layer_toml(config, config_file, is_built_in, role_name).await?;
    if role_layer_toml
        .as_table()
        .is_some_and(toml::map::Map::is_empty)
    {
        return Ok(());
    }
    let preserve_current_provider = role_layer_toml.get("model_provider").is_none();
    let preserve_current_service_tier = role_layer_toml.get("service_tier").is_none();

    *config = reload::build_next_config(
        config,
        role_layer_toml,
        developer_instructions,
        preserve_current_provider,
        preserve_current_service_tier,
    )
    .await?;
    Ok(())
}

async fn load_role_layer_toml(
    config: &Config,
    config_file: &Path,
    is_built_in: bool,
    role_name: &str,
) -> anyhow::Result<TomlValue> {
    let (role_config_toml, role_config_base) = if is_built_in {
        let role_config_contents = built_in::config_file_contents(config_file)
            .map(str::to_owned)
            .ok_or(anyhow!("No corresponding config content"))?;
        let role_config_toml: TomlValue = toml::from_str(&role_config_contents)?;
        (role_config_toml, config.codex_home.as_path())
    } else {
        let role_config_contents = tokio::fs::read_to_string(config_file).await?;
        let role_config_base = config_file
            .parent()
            .ok_or(anyhow!("No corresponding config content"))?;
        let role_config_toml = parse_agent_role_file_contents(
            &role_config_contents,
            config_file,
            role_config_base,
            Some(role_name),
        )?
        .config;
        (role_config_toml, role_config_base)
    };

    deserialize_config_toml_with_base(role_config_toml.clone(), role_config_base)?;
    Ok(resolve_relative_paths_in_config_toml(
        role_config_toml,
        role_config_base,
    )?)
}

pub(crate) fn resolve_role_config<'a>(
    config: &'a Config,
    role_name: &str,
) -> Option<&'a AgentRoleConfig> {
    if !agent_selector_enabled(config, role_name) {
        return None;
    }
    config
        .agent_roles
        .get(role_name)
        .or_else(|| built_in::configs().get(role_name))
}

pub(crate) fn resolve_role_config_owned(
    config: &Config,
    role_name: &str,
) -> Option<AgentRoleConfig> {
    if !agent_selector_enabled(config, role_name) {
        return None;
    }
    resolve_role_config(config, role_name).cloned().or_else(|| {
        built_in::external_agent_role_config_with_override(
            role_name,
            config
                .agent_selector_overrides
                .get(role_name)
                .and_then(|override_config| override_config.enabled),
        )
    })
}

pub fn agent_selector_enabled(config: &Config, selector: &str) -> bool {
    if let Some(spec) = codex_config::agent_defaults::agent_model_spec(selector) {
        if let Some(enabled) = config
            .agent_selector_overrides
            .get(selector)
            .and_then(|override_config| override_config.enabled)
        {
            return enabled;
        }
        if let Some(enabled) = config
            .agent_selector_overrides
            .get(spec.slug)
            .and_then(|override_config| override_config.enabled)
        {
            return enabled;
        }
        return spec.is_enabled();
    }
    if crate::agent::external_capabilities::looks_like_antigravity_selector(selector) {
        if let Some(enabled) = config
            .agent_selector_overrides
            .get(selector)
            .and_then(|override_config| override_config.enabled)
        {
            return enabled;
        }
        let configured_provider_model = config
            .agent_selector_overrides
            .get("antigravity")
            .and_then(|override_config| override_config.model.as_deref())
            .is_some_and(|model| selector == format!("antigravity-{model}"));
        return configured_provider_model
            && config
                .agent_selector_overrides
                .get("antigravity")
                .and_then(|override_config| override_config.enabled)
                .unwrap_or(true);
    }
    true
}

pub(crate) fn dynamic_antigravity_role_config(
    config: &Config,
    selector: &str,
    effort: Option<&str>,
) -> Option<AgentRoleConfig> {
    configured_antigravity_role_config(config, selector, /*model_override*/ None, effort)
}

fn configured_antigravity_role_config(
    config: &Config,
    selector: &str,
    model_override: Option<&str>,
    effort: Option<&str>,
) -> Option<AgentRoleConfig> {
    let model = selector.strip_prefix("antigravity-").or(model_override);
    if let Some(model) = model
        && !crate::agent::external_capabilities::is_valid_antigravity_model_name(model)
    {
        return None;
    }
    if selector != "antigravity" && model.is_none() {
        return None;
    }
    let base = config.agent_roles.get("antigravity").cloned().or_else(|| {
        built_in::external_agent_role_config_with_override("antigravity", Some(true))
    })?;
    let Some(AgentRoleBackendConfig::ExternalCommand(mut backend)) = base.backend else {
        return None;
    };
    if let Some(model) = model {
        replace_backend_argument(&mut backend.args, "--model", model);
    }
    if let Some(effort) = effort {
        replace_backend_argument(&mut backend.args, "--effort", effort);
    }
    Some(AgentRoleConfig {
        description: Some(if selector == "antigravity" {
            "Antigravity provider-default selector with configured defaults.".to_string()
        } else {
            format!("Antigravity discovered model selector `{selector}`.")
        }),
        config_file: None,
        nickname_candidates: None,
        backend: Some(AgentRoleBackendConfig::ExternalCommand(backend)),
    })
}

fn replace_backend_argument(args: &mut Vec<String>, flag: &str, value: &str) {
    let inline_prefix = format!("{flag}=");
    let mut retained = Vec::with_capacity(args.len() + 2);
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        if argument == flag {
            index += 1;
            if index < args.len() && !args[index].starts_with("--") {
                index += 1;
            }
            continue;
        }
        if argument.starts_with(&inline_prefix) {
            index += 1;
            continue;
        }
        retained.push(argument.clone());
        index += 1;
    }
    retained.extend([flag.to_string(), value.to_string()]);
    *args = retained;
}

pub(crate) fn install_dynamic_antigravity_role(
    config: &mut Config,
    selector: &str,
    effort: Option<&str>,
) -> Result<(), String> {
    let role = dynamic_antigravity_role_config(config, selector, effort).ok_or_else(|| {
        format!("Unable to construct external selector `{selector}` without substitution.")
    })?;
    config.agent_roles.insert(selector.to_string(), role);
    Ok(())
}

pub(crate) fn install_configured_antigravity_role(
    config: &mut Config,
    selector: &str,
    model_override: Option<&str>,
    effort: Option<&str>,
) -> Result<(), String> {
    let role = configured_antigravity_role_config(config, selector, model_override, effort)
        .ok_or_else(|| {
            format!("Unable to construct external selector `{selector}` without substitution.")
        })?;
    config.agent_roles.insert(selector.to_string(), role);
    Ok(())
}

pub(crate) fn install_external_role_defaults(
    config: &mut Config,
    selector: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<(), String> {
    let mut role = config
        .agent_roles
        .get(selector)
        .cloned()
        .or_else(|| built_in::external_agent_role_config_with_override(selector, Some(true)))
        .ok_or_else(|| format!("Unable to configure external selector `{selector}`."))?;
    let Some(AgentRoleBackendConfig::ExternalCommand(backend)) = role.backend.as_mut() else {
        return Err(format!("Selector `{selector}` is not an external agent."));
    };
    if let Some(model) = model {
        replace_backend_argument(&mut backend.args, "--model", model);
    }
    if let Some(effort) = effort {
        replace_backend_argument(&mut backend.args, "--effort", effort);
    }
    config.agent_roles.insert(selector.to_string(), role);
    Ok(())
}

pub(crate) fn external_agent_role_config(role_name: &str) -> Option<AgentRoleConfig> {
    built_in::external_agent_role_config_with_override(role_name, /*enabled_override*/ None)
}

/// Resolves an external-command backend regardless of selector enablement.
pub fn external_agent_backend_for_selector(
    config: &Config,
    selector: &str,
) -> Option<ExternalCommandAgentBackendConfig> {
    let role = config
        .agent_roles
        .get(selector)
        .cloned()
        .or_else(|| built_in::external_agent_role_config_with_override(selector, Some(true)))?;
    match role.backend? {
        AgentRoleBackendConfig::ExternalCommand(backend) => Some(backend),
    }
}

mod reload {
    use super::*;

    pub(super) async fn build_next_config(
        config: &Config,
        role_layer_toml: TomlValue,
        developer_instructions: RoleDeveloperInstructions,
        preserve_current_provider: bool,
        preserve_current_service_tier: bool,
    ) -> anyhow::Result<Config> {
        let preserve_current_model = role_layer_toml.get("model").is_none();
        let preserve_current_reasoning_effort =
            role_layer_toml.get("model_reasoning_effort").is_none();
        let preserve_current_base_instructions = role_layer_toml.get("instructions").is_none()
            && role_layer_toml.get("model_instructions_file").is_none();
        let mut overrides = reload_overrides(
            config,
            preserve_current_model,
            preserve_current_provider,
            preserve_current_service_tier,
        );
        if let (RoleDeveloperInstructions::PreserveCallerInstructions, Some(_), None) = (
            developer_instructions,
            &config.multi_agent_v2.subagent_developer_instructions,
            role_layer_toml.get("developer_instructions"),
        ) {
            overrides
                .developer_instructions
                .clone_from(&config.developer_instructions);
        }
        let config_layer_stack = build_config_layer_stack(config, &role_layer_toml)?;
        let merged_config = deserialize_effective_config(config, &config_layer_stack)?;

        let mut next_config = Config::load_config_with_layer_stack(
            LOCAL_FS.as_ref(),
            merged_config,
            overrides,
            config.codex_home.clone(),
            config_layer_stack,
        )
        .await?;
        if preserve_current_reasoning_effort {
            next_config
                .model_reasoning_effort
                .clone_from(&config.model_reasoning_effort);
        }
        if preserve_current_base_instructions {
            let personality_changed = config.personality != next_config.personality
                || config.features.enabled(Feature::Personality)
                    != next_config.features.enabled(Feature::Personality);
            if personality_changed
                && matches!(
                    config.base_instructions_provenance,
                    Some(BaseInstructionsProvenance::Model { .. })
                )
            {
                next_config.base_instructions = None;
                next_config.base_instructions_provenance = None;
            } else {
                next_config.base_instructions = config.base_instructions.clone();
                next_config.base_instructions_provenance =
                    config.base_instructions_provenance.clone();
            }
        }
        Ok(next_config)
    }

    fn build_config_layer_stack(
        config: &Config,
        role_layer_toml: &TomlValue,
    ) -> anyhow::Result<ConfigLayerStack> {
        let mut layers = existing_layers(config);
        insert_layer(&mut layers, role_layer(role_layer_toml.clone()));
        Ok(ConfigLayerStack::new(
            layers,
            config.config_layer_stack.requirements().clone(),
            config.config_layer_stack.requirements_toml().clone(),
        )?)
    }

    fn deserialize_effective_config(
        config: &Config,
        config_layer_stack: &ConfigLayerStack,
    ) -> anyhow::Result<ConfigToml> {
        Ok(deserialize_config_toml_with_base(
            config_layer_stack.effective_config(),
            &config.codex_home,
        )?)
    }

    fn existing_layers(config: &Config) -> Vec<ConfigLayerEntry> {
        config
            .config_layer_stack
            .all_layers_low_to_high()
            .cloned()
            .collect()
    }

    fn insert_layer(layers: &mut Vec<ConfigLayerEntry>, layer: ConfigLayerEntry) {
        let insertion_index =
            layers.partition_point(|existing_layer| existing_layer.name <= layer.name);
        layers.insert(insertion_index, layer);
    }

    fn role_layer(role_layer_toml: TomlValue) -> ConfigLayerEntry {
        ConfigLayerEntry::new(ConfigLayerSource::SessionFlags, role_layer_toml)
    }

    fn reload_overrides(
        config: &Config,
        preserve_current_model: bool,
        preserve_current_provider: bool,
        preserve_current_service_tier: bool,
    ) -> ConfigOverrides {
        ConfigOverrides {
            cwd: Some(config.cwd.to_path_buf()),
            model: preserve_current_model
                .then(|| config.model.clone())
                .flatten(),
            model_provider: preserve_current_provider.then(|| config.model_provider_id.clone()),
            service_tier: preserve_current_service_tier.then(|| config.service_tier.clone()),
            codex_linux_sandbox_exe: config.codex_linux_sandbox_exe.clone(),
            main_execve_wrapper_exe: config.main_execve_wrapper_exe.clone(),
            ..Default::default()
        }
    }
}

pub(crate) mod spawn_tool_spec {
    use super::*;

    /// Builds the spawn-agent tool description text from built-in and configured roles.
    pub(crate) fn build(user_defined_agent_roles: &BTreeMap<String, AgentRoleConfig>) -> String {
        build_with_external_selectors(user_defined_agent_roles, &[])
    }

    pub(crate) fn build_with_external_selectors(
        user_defined_agent_roles: &BTreeMap<String, AgentRoleConfig>,
        selectors: &[String],
    ) -> String {
        let built_in_roles = built_in::configs();
        let external_agent_roles = built_in::external_agent_configs();
        let mut description = build_from_configs(
            built_in_roles,
            external_agent_roles,
            user_defined_agent_roles,
        );
        if !selectors.is_empty() {
            description.push_str("\n\nDiscovered external selectors:\n");
            description.push_str(
                &selectors
                    .iter()
                    .map(|selector| format!("- `{selector}`: Antigravity model selector."))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        description
    }

    pub(crate) fn build_for_config_with_external_selectors(
        config: &Config,
        selectors: &[String],
    ) -> String {
        let built_in_roles = built_in::configs();
        let external_agent_roles = built_in::external_agent_configs();
        let enabled_user_roles = config
            .agent_roles
            .iter()
            .filter(|(name, _)| agent_selector_enabled(config, name))
            .map(|(name, role)| (name.clone(), role.clone()))
            .collect();
        let mut enabled_external_roles = external_agent_roles
            .iter()
            .filter(|(name, _)| agent_selector_enabled(config, name))
            .map(|(name, role)| (name.clone(), role.clone()))
            .collect::<BTreeMap<_, _>>();
        for (selector, override_config) in &config.agent_selector_overrides {
            if override_config.enabled == Some(true)
                && !enabled_external_roles.contains_key(selector)
                && let Some(role) =
                    built_in::external_agent_role_config_with_override(selector, Some(true))
            {
                enabled_external_roles.insert(selector.clone(), role);
            }
        }
        let enabled_selectors = selectors
            .iter()
            .filter(|selector| agent_selector_enabled(config, selector))
            .cloned()
            .collect::<Vec<_>>();
        let mut description =
            build_from_configs(built_in_roles, &enabled_external_roles, &enabled_user_roles);
        if !enabled_selectors.is_empty() {
            description.push_str("\n\nDiscovered external selectors:\n");
            description.push_str(
                &enabled_selectors
                    .iter()
                    .map(|selector| format!("- `{selector}`: Antigravity model selector."))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        description
    }

    // This function is not inlined for testing purpose.
    fn build_from_configs(
        built_in_roles: &BTreeMap<String, AgentRoleConfig>,
        external_agent_roles: &BTreeMap<String, AgentRoleConfig>,
        user_defined_roles: &BTreeMap<String, AgentRoleConfig>,
    ) -> String {
        let mut seen = BTreeSet::new();
        let mut formatted_roles = Vec::new();
        for (name, declaration) in user_defined_roles {
            if seen.insert(name.as_str()) {
                formatted_roles.push(format_role(name, declaration));
            }
        }
        for (name, declaration) in built_in_roles {
            if seen.insert(name.as_str()) {
                formatted_roles.push(format_role(name, declaration));
            }
        }
        for (name, declaration) in external_agent_roles {
            if seen.insert(name.as_str()) {
                formatted_roles.push(format_role(name, declaration));
            }
        }

        format!("Available roles:\n{}", formatted_roles.join("\n"))
    }

    fn format_role(name: &str, declaration: &AgentRoleConfig) -> String {
        if let Some(description) = &declaration.description {
            let locked_settings_note = declaration
                .config_file
                .as_ref()
                .and_then(|config_file| {
                    built_in::config_file_contents(config_file)
                        .map(str::to_owned)
                        .or_else(|| std::fs::read_to_string(config_file).ok())
                })
                .and_then(|contents| toml::from_str::<TomlValue>(&contents).ok())
                .map(|role_toml| {
                    let model = role_toml
                        .get("model")
                        .and_then(TomlValue::as_str);
                    let reasoning_effort = role_toml
                        .get("model_reasoning_effort")
                        .and_then(TomlValue::as_str);
                    let service_tier = role_toml
                        .get("service_tier")
                        .and_then(TomlValue::as_str);

                    let model_and_reasoning_note = match (model, reasoning_effort) {
                        (Some(model), Some(reasoning_effort)) => format!(
                            "\n- This role's model is set to `{model}` and its reasoning effort is set to `{reasoning_effort}`. These settings cannot be changed."
                        ),
                        (Some(model), None) => {
                            format!(
                                "\n- This role's model is set to `{model}` and cannot be changed."
                            )
                        }
                        (None, Some(reasoning_effort)) => {
                            format!(
                                "\n- This role's reasoning effort is set to `{reasoning_effort}` and cannot be changed."
                            )
                        }
                        (None, None) => String::new(),
                    };
                    let service_tier_note = service_tier
                        .map(|service_tier| {
                            format!(
                                "\n- This role's service tier is set to `{service_tier}`. If it is supported by the resolved model, it takes precedence over a valid spawn request service tier."
                            )
                        })
                        .unwrap_or_default();
                    format!("{model_and_reasoning_note}{service_tier_note}")
                })
                .unwrap_or_default();
            format!("{name}: {{\n{description}{locked_settings_note}\n}}")
        } else {
            format!("{name}: no description")
        }
    }
}

mod built_in {
    use super::*;

    const BUILT_IN_EXTERNAL_AGENT_TIMEOUT_MS: u64 = 30 * 60 * 1000;

    /// Returns the cached built-in role declarations defined in this module.
    pub(super) fn configs() -> &'static BTreeMap<String, AgentRoleConfig> {
        static CONFIG: LazyLock<BTreeMap<String, AgentRoleConfig>> = LazyLock::new(|| {
            BTreeMap::from([
                (
                    DEFAULT_ROLE_NAME.to_string(),
                    AgentRoleConfig {
                        description: Some("Default agent.".to_string()),
                        config_file: None,
                        nickname_candidates: None,
                        backend: None,
                    }
                ),
                (
                    "explorer".to_string(),
                    AgentRoleConfig {
                        description: Some(r#"Use `explorer` for specific codebase questions.
Explorers are fast and authoritative.
They must be used to ask specific, well-scoped questions on the codebase.
Rules:
- In order to avoid redundant work, you should avoid exploring the same problem that explorers have already covered. Typically, you should trust the explorer results without additional verification. You are still allowed to inspect the code yourself to gain the needed context!
- You are encouraged to spawn up multiple explorers in parallel when you have multiple distinct questions to ask about the codebase that can be answered independently. This allows you to get more information faster without waiting for one question to finish before asking the next. While waiting for the explorer results, you can continue working on other local tasks that do not depend on those results. This parallelism is a key advantage of delegation, so use it whenever you have multiple questions to ask.
- Reuse existing explorers for related questions."#.to_string()),
                        config_file: Some("explorer.toml".to_string().parse().unwrap_or_default()),
                        nickname_candidates: None,
                        backend: None,
                    }
                ),
                (
                    "worker".to_string(),
                    AgentRoleConfig {
                        description: Some(r#"Use for execution and production work.
Typical tasks:
- Implement part of a feature
- Fix tests or bugs
- Split large refactors into independent chunks
Rules:
- Explicitly assign **ownership** of the task (files / responsibility). When the subtask involves code changes, you should clearly specify which files or modules the worker is responsible for. This helps avoid merge conflicts and ensures accountability. For example, you can say "Worker 1 is responsible for updating the authentication module, while Worker 2 will handle the database layer." By defining clear ownership, you can delegate more effectively and reduce coordination overhead.
- Always tell workers they are **not alone in the codebase**, and they should not revert the edits made by others, and they should adjust their implementation to accommodate the changes made by others. This is important because there may be multiple workers making changes in parallel, and they need to be aware of each other's work to avoid conflicts and ensure a cohesive final product."#.to_string()),
                        config_file: None,
                        nickname_candidates: None,
                        backend: None,
                    }
                ),
                // Awaiter is temp removed
//                 (
//                     "awaiter".to_string(),
//                     AgentRoleConfig {
//                         description: Some(r#"Use an `awaiter` agent EVERY TIME you must run a command that will take some very long time.
// This includes, but not only:
// * testing
// * monitoring of a long running process
// * explicit ask to wait for something
//
// Rules:
// - When an awaiter is running, you can work on something else. If you need to wait for its completion, use the largest possible timeout.
// - Be patient with the `awaiter`.
// - Do not use an awaiter for every compilation/test if it won't take time. Only use if for long running commands.
// - Close the awaiter when you're done with it."#.to_string()),
//                         config_file: Some("awaiter.toml".to_string().parse().unwrap_or_default()),
//                     }
//                 )
            ])
        });
        &CONFIG
    }

    pub(super) fn external_agent_configs() -> &'static BTreeMap<String, AgentRoleConfig> {
        static CONFIG: LazyLock<BTreeMap<String, AgentRoleConfig>> = LazyLock::new(|| {
            codex_config::agent_defaults::enabled_agent_model_specs()
                .into_iter()
                .map(|spec| {
                    (
                        spec.slug.to_string(),
                        external_agent_role_config_from_spec(spec),
                    )
                })
                .collect()
        });
        &CONFIG
    }

    pub(super) fn external_agent_role_config_with_override(
        role_name: &str,
        enabled_override: Option<bool>,
    ) -> Option<AgentRoleConfig> {
        external_agent_configs()
            .get(role_name)
            .cloned()
            .or_else(|| {
                codex_config::agent_defaults::agent_model_spec(role_name)
                    .filter(|spec| enabled_override.unwrap_or_else(|| spec.is_enabled()))
                    .map(external_agent_role_config_from_spec)
            })
    }

    fn external_agent_role_config_from_spec(
        spec: &'static codex_config::agent_defaults::AgentModelSpec,
    ) -> AgentRoleConfig {
        let defaults = codex_config::agent_defaults::agent_config_from_spec(spec);
        AgentRoleConfig {
            description: Some(spec.description.to_string()),
            config_file: None,
            nickname_candidates: None,
            backend: Some(AgentRoleBackendConfig::ExternalCommand(
                ExternalCommandAgentBackendConfig {
                    command: defaults.command,
                    protocol: ExternalCommandProtocol::RawCli,
                    args: defaults.args,
                    args_read_only: defaults.args_read_only.unwrap_or_default(),
                    args_write: defaults.args_write.unwrap_or_default(),
                    env: defaults.env.unwrap_or_default(),
                    timeout_ms: BUILT_IN_EXTERNAL_AGENT_TIMEOUT_MS,
                    launch_family: Some(spec.family.to_string()),
                },
            )),
        }
    }

    /// Resolves a built-in role `config_file` path to embedded content.
    pub(super) fn config_file_contents(path: &Path) -> Option<&'static str> {
        const EXPLORER: &str = include_str!("builtins/explorer.toml");
        const AWAITER: &str = include_str!("builtins/awaiter.toml");
        match path.to_str()? {
            "explorer.toml" => Some(EXPLORER),
            "awaiter.toml" => Some(AWAITER),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "role_tests.rs"]
mod tests;
