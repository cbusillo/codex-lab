use super::*;
use crate::config::ConfigBuilder;
use crate::plugins::plugins_manager_for_config;
use crate::skills_load_input_from_config;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::openai_models::ReasoningEffort;
use codex_skills_extension::HostSkillsService;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

async fn test_config_with_cli_overrides(
    cli_overrides: Vec<(String, TomlValue)>,
) -> (TempDir, Config) {
    let home = TempDir::new().expect("create temp dir");
    let home_path = home.path().to_path_buf();
    let config = ConfigBuilder::default()
        .codex_home(home_path.clone())
        .cli_overrides(cli_overrides)
        .fallback_cwd(Some(home_path))
        .build()
        .await
        .expect("load test config");
    (home, config)
}

async fn write_role_config(home: &TempDir, name: &str, contents: &str) -> PathBuf {
    let role_path = home.path().join(name);
    tokio::fs::write(&role_path, contents)
        .await
        .expect("write role config");
    role_path
}

fn session_flags_layer_count(config: &Config) -> usize {
    config
        .config_layer_stack
        .all_layers_low_to_high()
        .filter(|layer| layer.name == ConfigLayerSource::SessionFlags)
        .count()
}

#[tokio::test]
async fn apply_role_defaults_to_default_and_leaves_config_unchanged() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    let before = config.clone();

    apply_role_to_config(&mut config, /*role_name*/ None)
        .await
        .expect("default role should apply");

    assert_eq!(before, config);
}

#[tokio::test]
async fn apply_role_returns_error_for_unknown_role() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;

    let err = apply_role_to_config(&mut config, Some("missing-role"))
        .await
        .expect_err("unknown role should fail");

    assert_eq!(err, "unknown agent_type 'missing-role'");
}

#[tokio::test]
async fn apply_empty_explorer_role_preserves_current_model_and_reasoning_effort() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    let before_layers = session_flags_layer_count(&config);
    config.model = Some("gpt-5.4-mini".to_string());
    config.model_reasoning_effort = Some(ReasoningEffort::High);

    apply_role_to_config(&mut config, Some("explorer"))
        .await
        .expect("explorer role should apply");

    assert_eq!(config.model.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(session_flags_layer_count(&config), before_layers);
}

#[tokio::test]
async fn apply_role_returns_unavailable_for_missing_user_role_file() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    config.agent_roles.insert(
        "custom".to_string(),
        AgentRoleConfig {
            description: None,
            config_file: Some(PathBuf::from("/path/does/not/exist.toml")),
            nickname_candidates: None,
            backend: None,
        },
    );

    let err = apply_role_to_config(&mut config, Some("custom"))
        .await
        .expect_err("missing role file should fail");

    assert_eq!(err, AGENT_TYPE_UNAVAILABLE_ERROR);
}

#[tokio::test]
async fn apply_role_returns_unavailable_for_invalid_user_role_toml() {
    let (home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    let role_path = write_role_config(&home, "invalid-role.toml", "model = [").await;
    config.agent_roles.insert(
        "custom".to_string(),
        AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    );

    let err = apply_role_to_config(&mut config, Some("custom"))
        .await
        .expect_err("invalid role file should fail");

    assert_eq!(err, AGENT_TYPE_UNAVAILABLE_ERROR);
}

#[tokio::test]
async fn apply_role_ignores_agent_metadata_fields_in_user_role_file() {
    let (home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    let role_path = write_role_config(
        &home,
        "metadata-role.toml",
        r#"
name = "archivist"
description = "Role metadata"
nickname_candidates = ["Hypatia"]
developer_instructions = "Stay focused"
model = "role-model"
"#,
    )
    .await;
    config.agent_roles.insert(
        "custom".to_string(),
        AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    );

    apply_role_to_config(&mut config, Some("custom"))
        .await
        .expect("custom role should apply");

    assert_eq!(config.model.as_deref(), Some("role-model"));
}

#[tokio::test]
async fn apply_role_preserves_unspecified_keys() {
    let (home, mut config) = test_config_with_cli_overrides(vec![(
        "model".to_string(),
        TomlValue::String("base-model".to_string()),
    )])
    .await;
    config.codex_linux_sandbox_exe = Some(PathBuf::from("/tmp/codex-linux-sandbox"));
    config.main_execve_wrapper_exe = Some(PathBuf::from("/tmp/codex-execve-wrapper"));
    let role_path = write_role_config(
        &home,
        "instructions-only.toml",
        "developer_instructions = \"Stay focused\"",
    )
    .await;
    config.agent_roles.insert(
        "custom".to_string(),
        AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    );

    config.model = Some("spawn-model".to_string());
    config.model_reasoning_effort = Some(ReasoningEffort::Low);
    config.base_instructions = Some("inherited model instructions".to_string());
    config.base_instructions_provenance = Some(BaseInstructionsProvenance::Model {
        model: "parent-model".to_string(),
    });
    let base_instructions = config.base_instructions.clone();
    let provenance = config.base_instructions_provenance.clone();

    apply_role_to_config(&mut config, Some("custom"))
        .await
        .expect("custom role should apply");

    assert_eq!(
        (config.model.as_deref(), config.model_reasoning_effort),
        (Some("spawn-model"), Some(ReasoningEffort::Low)),
    );
    assert_eq!(
        config.codex_linux_sandbox_exe,
        Some(PathBuf::from("/tmp/codex-linux-sandbox"))
    );
    assert_eq!(
        config.main_execve_wrapper_exe,
        Some(PathBuf::from("/tmp/codex-execve-wrapper"))
    );
    assert_eq!(config.base_instructions, base_instructions);
    assert_eq!(config.base_instructions_provenance, provenance);
}

#[tokio::test]
async fn apply_role_regenerates_model_instructions_when_personality_changes() {
    for (role_contents, provenance) in [
        (
            "personality = \"none\"",
            BaseInstructionsProvenance::Model {
                model: "parent-model".to_string(),
            },
        ),
        (
            "[features]\npersonality = false",
            BaseInstructionsProvenance::Model {
                model: "parent-model".to_string(),
            },
        ),
        ("personality = \"none\"", BaseInstructionsProvenance::Custom),
    ] {
        let (home, mut config) = test_config_with_cli_overrides(vec![
            (
                "personality".to_string(),
                TomlValue::String("friendly".to_string()),
            ),
            ("features.personality".to_string(), TomlValue::Boolean(true)),
        ])
        .await;
        let role_path = write_role_config(&home, "personality-role.toml", role_contents).await;
        config.agent_roles.insert(
            "custom".to_string(),
            AgentRoleConfig {
                description: None,
                config_file: Some(role_path),
                nickname_candidates: None,
                backend: None,
            },
        );
        config.base_instructions = Some("inherited instructions".to_string());
        config.base_instructions_provenance = Some(provenance.clone());

        apply_role_to_config(&mut config, Some("custom"))
            .await
            .expect("custom role should apply");

        let expected = match provenance {
            BaseInstructionsProvenance::Model { .. } => (None, None),
            BaseInstructionsProvenance::Custom => (
                Some("inherited instructions".to_string()),
                Some(BaseInstructionsProvenance::Custom),
            ),
        };
        assert_eq!(
            (
                config.base_instructions,
                config.base_instructions_provenance
            ),
            expected
        );
    }
}

#[tokio::test]
async fn apply_role_reports_explicit_service_tier() {
    let (home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    let role_path = write_role_config(
        &home,
        "tiered-role.toml",
        r#"developer_instructions = "Stay focused"
service_tier = "priority"
"#,
    )
    .await;
    config.agent_roles.insert(
        "custom".to_string(),
        AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    );

    apply_role_to_config(&mut config, Some("custom"))
        .await
        .expect("custom role should apply");

    assert_eq!(
        config.service_tier,
        Some(ServiceTier::Fast.request_value().to_string())
    );
}

#[tokio::test]
async fn apply_role_preserves_existing_service_tier_without_override() {
    let (home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    config.service_tier = Some(ServiceTier::Fast.request_value().to_string());
    let role_path = write_role_config(
        &home,
        "default-tier-role.toml",
        r#"developer_instructions = "Stay focused"
"#,
    )
    .await;
    config.agent_roles.insert(
        "custom".to_string(),
        AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    );

    apply_role_to_config(&mut config, Some("custom"))
        .await
        .expect("custom role should apply");

    assert_eq!(
        config.service_tier,
        Some(ServiceTier::Fast.request_value().to_string())
    );
}

#[tokio::test]
#[cfg(not(windows))]
async fn apply_role_does_not_materialize_default_sandbox_workspace_write_fields() {
    use codex_protocol::protocol::SandboxPolicy;
    let (home, mut config) = test_config_with_cli_overrides(vec![
        (
            "sandbox_mode".to_string(),
            TomlValue::String("workspace-write".to_string()),
        ),
        (
            "sandbox_workspace_write.network_access".to_string(),
            TomlValue::Boolean(true),
        ),
    ])
    .await;
    let role_path = write_role_config(
        &home,
        "sandbox-role.toml",
        r#"developer_instructions = "Stay focused"

[sandbox_workspace_write]
writable_roots = ["./sandbox-root"]
"#,
    )
    .await;
    config.agent_roles.insert(
        "custom".to_string(),
        AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    );

    apply_role_to_config(&mut config, Some("custom"))
        .await
        .expect("custom role should apply");

    let role_layer = config
        .config_layer_stack
        .all_layers_low_to_high()
        .rfind(|layer| layer.name == ConfigLayerSource::SessionFlags)
        .expect("expected a session flags layer");
    let sandbox_workspace_write = role_layer
        .config
        .get("sandbox_workspace_write")
        .and_then(TomlValue::as_table)
        .expect("role layer should include sandbox_workspace_write");
    assert_eq!(
        sandbox_workspace_write.contains_key("network_access"),
        false
    );
    assert_eq!(
        sandbox_workspace_write.contains_key("exclude_tmpdir_env_var"),
        false
    );
    assert_eq!(
        sandbox_workspace_write.contains_key("exclude_slash_tmp"),
        false
    );

    match &config.legacy_sandbox_policy() {
        SandboxPolicy::WorkspaceWrite { network_access, .. } => {
            assert_eq!(*network_access, true);
        }
        other => panic!("expected workspace-write sandbox policy, got {other:?}"),
    }
}

#[tokio::test]
async fn apply_role_takes_precedence_over_existing_session_flags_for_same_key() {
    let (home, mut config) = test_config_with_cli_overrides(vec![(
        "model".to_string(),
        TomlValue::String("cli-model".to_string()),
    )])
    .await;
    let before_layers = session_flags_layer_count(&config);
    let role_path = write_role_config(
        &home,
        "model-role.toml",
        "developer_instructions = \"Stay focused\"\nmodel = \"role-model\"",
    )
    .await;
    config.agent_roles.insert(
        "custom".to_string(),
        AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    );

    apply_role_to_config(&mut config, Some("custom"))
        .await
        .expect("custom role should apply");

    assert_eq!(config.model.as_deref(), Some("role-model"));
    assert_eq!(session_flags_layer_count(&config), before_layers + 1);
}

#[cfg_attr(windows, ignore)]
#[tokio::test]
async fn apply_role_skills_config_disables_skill_for_spawned_agent() {
    let (home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    let skill_dir = home.path().join("skills").join("demo");
    fs::create_dir_all(&skill_dir).expect("create skill dir");
    let skill_path = skill_dir.join("SKILL.md");
    fs::write(
        &skill_path,
        "---\nname: demo-skill\ndescription: demo description\n---\n\n# Body\n",
    )
    .expect("write skill");
    let role_path = write_role_config(
        &home,
        "skills-role.toml",
        &format!(
            r#"developer_instructions = "Stay focused"

[[skills.config]]
path = "{}"
enabled = false
"#,
            skill_path.display()
        ),
    )
    .await;
    config.agent_roles.insert(
        "custom".to_string(),
        AgentRoleConfig {
            description: None,
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    );

    apply_role_to_config(&mut config, Some("custom"))
        .await
        .expect("custom role should apply");

    let plugins_manager = Arc::new(plugins_manager_for_config(&config));
    let skills_service =
        HostSkillsService::new(home.path().abs(), /*bundled_skills_enabled*/ true);
    let plugins_input = config.plugins_config_input();
    let plugin_outcome = plugins_manager.plugins_for_config(&plugins_input).await;
    let effective_skill_roots = plugin_outcome.effective_plugin_skill_roots();
    let plugin_skill_snapshots = plugins_manager.plugin_skill_snapshots_for_config(&plugins_input);
    let skills_input = skills_load_input_from_config(&config, effective_skill_roots)
        .with_plugin_skill_snapshots(plugin_skill_snapshots);
    let snapshot = skills_service
        .snapshot_for_config(
            &skills_input,
            Some(Arc::clone(&codex_exec_server::LOCAL_FS)),
        )
        .await;
    let outcome = snapshot.outcome();
    let skill = outcome
        .skills
        .iter()
        .find(|skill| skill.name == "demo-skill")
        .expect("demo skill should be discovered");

    assert_eq!(outcome.is_skill_enabled(skill), false);
}

#[test]
fn spawn_tool_spec_build_deduplicates_user_defined_built_in_roles() {
    let user_defined_roles = BTreeMap::from([
        (
            "explorer".to_string(),
            AgentRoleConfig {
                description: Some("user override".to_string()),
                config_file: None,
                nickname_candidates: None,
                backend: None,
            },
        ),
        ("researcher".to_string(), AgentRoleConfig::default()),
    ]);

    let spec = spawn_tool_spec::build(&user_defined_roles);

    assert!(spec.contains("researcher: no description"));
    assert!(spec.contains("explorer: {\nuser override\n}"));
    assert!(spec.contains("default: {\nDefault agent.\n}"));
    assert!(!spec.contains("Explorers are fast and authoritative."));
}

#[test]
fn spawn_tool_spec_lists_user_defined_roles_before_built_ins() {
    let user_defined_roles = BTreeMap::from([(
        "aaa".to_string(),
        AgentRoleConfig {
            description: Some("first".to_string()),
            config_file: None,
            nickname_candidates: None,
            backend: None,
        },
    )]);

    let spec = spawn_tool_spec::build(&user_defined_roles);
    let user_index = spec.find("aaa: {\nfirst\n}").expect("find user role");
    let built_in_index = spec
        .find("default: {\nDefault agent.\n}")
        .expect("find built-in role");

    assert!(user_index < built_in_index);
}

#[test]
fn spawn_tool_spec_marks_role_locked_model_and_reasoning_effort() {
    let tempdir = TempDir::new().expect("create temp dir");
    let role_path = tempdir.path().join("researcher.toml");
    fs::write(
            &role_path,
            "developer_instructions = \"Research carefully\"\nmodel = \"gpt-5\"\nmodel_reasoning_effort = \"high\"\n",
        )
        .expect("write role config");
    let user_defined_roles = BTreeMap::from([(
        "researcher".to_string(),
        AgentRoleConfig {
            description: Some("Research carefully.".to_string()),
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    )]);

    let spec = spawn_tool_spec::build(&user_defined_roles);

    assert!(spec.contains(
            "Research carefully.\n- This role's model is set to `gpt-5` and its reasoning effort is set to `high`. These settings cannot be changed."
        ));
}

#[test]
fn spawn_tool_spec_marks_role_locked_reasoning_effort_only() {
    let tempdir = TempDir::new().expect("create temp dir");
    let role_path = tempdir.path().join("reviewer.toml");
    fs::write(
        &role_path,
        "developer_instructions = \"Review carefully\"\nmodel_reasoning_effort = \"medium\"\n",
    )
    .expect("write role config");
    let user_defined_roles = BTreeMap::from([(
        "reviewer".to_string(),
        AgentRoleConfig {
            description: Some("Review carefully.".to_string()),
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    )]);

    let spec = spawn_tool_spec::build(&user_defined_roles);

    assert!(spec.contains(
            "Review carefully.\n- This role's reasoning effort is set to `medium` and cannot be changed."
        ));
}

#[test]
fn spawn_tool_spec_marks_role_locked_service_tier() {
    let tempdir = TempDir::new().expect("create temp dir");
    let role_path = tempdir.path().join("tiered.toml");
    fs::write(
        &role_path,
        "developer_instructions = \"Stay fast\"\nservice_tier = \"priority\"\n",
    )
    .expect("write role config");
    let user_defined_roles = BTreeMap::from([(
        "tiered".to_string(),
        AgentRoleConfig {
            description: Some("Stay fast.".to_string()),
            config_file: Some(role_path),
            nickname_candidates: None,
            backend: None,
        },
    )]);

    let spec = spawn_tool_spec::build(&user_defined_roles);

    assert!(spec.contains(
        "Stay fast.\n- This role's service tier is set to `priority`. If it is supported by the resolved model, it takes precedence over a valid spawn request service tier."
    ));
}

#[tokio::test]
async fn dynamic_antigravity_role_carries_model_and_effort_flags() {
    let (_home, config) = test_config_with_cli_overrides(Vec::new()).await;
    let role = super::dynamic_antigravity_role_config(
        &config,
        "antigravity-gemini-3.6-flash-high",
        Some("high"),
    )
    .expect("dynamic selector should resolve");
    let Some(AgentRoleBackendConfig::ExternalCommand(backend)) = role.backend else {
        panic!("dynamic selector should use external command");
    };
    assert!(
        backend
            .args
            .windows(2)
            .any(|args| args == ["--model", "gemini-3.6-flash-high"])
    );
    assert!(
        backend
            .args
            .windows(2)
            .any(|args| args == ["--effort", "high"])
    );
}

#[tokio::test]
async fn dynamic_antigravity_role_preserves_configured_backend() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    let mut backend = ExternalCommandAgentBackendConfig {
        command: "custom-agy".to_string(),
        args: ["--custom", "--model=old-model", "--effort", "low"]
            .map(str::to_string)
            .to_vec(),
        ..Default::default()
    };
    backend
        .env
        .insert("CUSTOM_AGY".to_string(), "enabled".to_string());
    config.agent_roles.insert(
        "antigravity".to_string(),
        AgentRoleConfig {
            description: Some("Custom Antigravity backend.".to_string()),
            config_file: None,
            nickname_candidates: None,
            backend: Some(AgentRoleBackendConfig::ExternalCommand(backend)),
        },
    );

    let role = super::dynamic_antigravity_role_config(
        &config,
        "antigravity-gemini-3.6-flash-high",
        Some("high"),
    )
    .expect("dynamic selector should resolve from configured backend");
    let Some(AgentRoleBackendConfig::ExternalCommand(backend)) = role.backend else {
        panic!("dynamic selector should use external command");
    };

    assert_eq!(backend.command, "custom-agy");
    assert_eq!(
        backend.env.get("CUSTOM_AGY").map(String::as_str),
        Some("enabled")
    );
    assert_eq!(
        backend.args,
        [
            "--custom",
            "--model",
            "gemini-3.6-flash-high",
            "--effort",
            "high",
        ]
        .map(str::to_string)
    );
}

#[tokio::test]
async fn installed_dynamic_antigravity_role_is_resolvable_before_routing() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    let selector = "antigravity-gemini-3.6-flash-high";
    config.agent_selector_overrides.insert(
        selector.to_string(),
        codex_config::config_toml::AgentSelectorToml {
            enabled: Some(true),
            ..Default::default()
        },
    );

    super::install_dynamic_antigravity_role(&mut config, selector, Some("high"))
        .expect("dynamic selector should install");

    let role = super::resolve_role_config_owned(&config, selector)
        .expect("installed dynamic selector should resolve");
    let Some(AgentRoleBackendConfig::ExternalCommand(backend)) = role.backend else {
        panic!("installed dynamic selector should use the external backend");
    };
    assert!(
        backend
            .args
            .windows(2)
            .any(|args| args == ["--model", "gemini-3.6-flash-high"])
    );
    assert!(
        backend
            .args
            .windows(2)
            .any(|args| args == ["--effort", "high"])
    );
}

#[tokio::test]
async fn discovered_antigravity_selectors_enable_from_the_active_catalog() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    let selector = "antigravity-gemini-3.6-flash-high";

    assert!(super::agent_selector_enabled(&config, "antigravity"));
    assert!(!super::agent_selector_enabled(&config, selector));

    let backend = super::external_agent_backend_for_selector(&config, "antigravity")
        .expect("Antigravity backend");
    crate::agent::external_capabilities::record_active_capability_catalog(
        &backend,
        config.cwd.as_path(),
        &crate::agent::external_capabilities::ExternalAgentCapabilities {
            cli_family: "antigravity".to_string(),
            cli_version: None,
            supports_model_selection: true,
            supports_effort_selection: false,
            supported_flags: Default::default(),
            models: vec![
                crate::agent::external_capabilities::ExternalAgentModelCapability {
                    selector: selector.to_string(),
                    model: "gemini-3.6-flash-high".to_string(),
                    explicit_only: false,
                },
            ],
            effort_levels: Vec::new(),
            source: crate::agent::external_capabilities::ExternalAgentCapabilitySource::LocalCli,
            freshness: crate::agent::external_capabilities::ExternalAgentCapabilityFreshness::Fresh,
            observed_at_unix_seconds: 0,
            failure: None,
        },
    );
    assert!(super::agent_selector_enabled(&config, selector));

    config.agent_selector_overrides.insert(
        "antigravity".to_string(),
        codex_config::config_toml::AgentSelectorToml {
            enabled: Some(false),
            ..Default::default()
        },
    );
    assert!(!super::agent_selector_enabled(&config, selector));
    assert_eq!(
        super::antigravity_selector_rejection(&config, selector),
        None
    );

    config.agent_selector_overrides.insert(
        selector.to_string(),
        codex_config::config_toml::AgentSelectorToml {
            enabled: Some(true),
            ..Default::default()
        },
    );
    assert!(super::agent_selector_enabled(&config, selector));
}

#[tokio::test]
async fn configured_antigravity_provider_model_remains_enabled() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    config.agent_selector_overrides.insert(
        "antigravity".to_string(),
        codex_config::config_toml::AgentSelectorToml {
            enabled: Some(true),
            model: Some("gemini-3.6-flash-high".to_string()),
            effort: None,
        },
    );

    assert!(super::agent_selector_enabled(
        &config,
        "antigravity-gemini-3.6-flash-high"
    ));
}

#[tokio::test]
async fn selector_overrides_disable_static_and_discovered_external_agents() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    config.agent_selector_overrides.insert(
        "claude-sonnet-4.6".to_string(),
        codex_config::config_toml::AgentSelectorToml {
            enabled: Some(false),
            ..Default::default()
        },
    );
    config.agent_selector_overrides.insert(
        "antigravity".to_string(),
        codex_config::config_toml::AgentSelectorToml {
            enabled: Some(false),
            ..Default::default()
        },
    );

    assert!(super::resolve_role_config_owned(&config, "claude-sonnet-4.6").is_none());
    assert!(
        super::resolve_role_config_owned(&config, "antigravity-gemini-3.6-flash-high").is_none()
    );
    let description = spawn_tool_spec::build_for_config_with_external_selectors(
        &config,
        &["antigravity-gemini-3.6-flash-high".to_string()],
    );
    assert!(!description.contains("claude-sonnet-4.6"));
    assert!(!description.contains("antigravity-gemini-3.6-flash-high"));
}

#[tokio::test]
async fn selector_overrides_can_enable_a_gated_external_agent() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    config.agent_selector_overrides.insert(
        "cloud-gpt-5.1-codex-max".to_string(),
        codex_config::config_toml::AgentSelectorToml {
            enabled: Some(true),
            ..Default::default()
        },
    );

    assert!(super::resolve_role_config_owned(&config, "cloud-gpt-5.1-codex-max").is_some());
    let description = spawn_tool_spec::build_for_config_with_external_selectors(&config, &[]);
    assert!(description.contains("cloud-gpt-5.1-codex-max"));
}

#[tokio::test]
async fn selector_overrides_do_not_disable_native_roles() {
    let (_home, mut config) = test_config_with_cli_overrides(Vec::new()).await;
    config.agent_selector_overrides.insert(
        "explorer".to_string(),
        codex_config::config_toml::AgentSelectorToml {
            enabled: Some(false),
            ..Default::default()
        },
    );

    assert!(super::resolve_role_config_owned(&config, "explorer").is_some());
    let description = spawn_tool_spec::build_for_config_with_external_selectors(&config, &[]);
    assert!(description.contains("explorer"));
}

#[test]
fn built_in_config_file_contents_resolves_explorer_only() {
    assert_eq!(
        built_in::config_file_contents(Path::new("missing.toml")),
        None
    );
}
