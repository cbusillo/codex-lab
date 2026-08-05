use super::*;
use crate::config::AgentRoleBackendConfig;
use crate::config::ConfigBuilder;
use codex_config::config_toml::AgentSelectorToml;
use tempfile::TempDir;

async fn test_config() -> (TempDir, Config) {
    let home = TempDir::new().expect("create temp dir");
    let path = home.path().to_path_buf();
    let config = ConfigBuilder::default()
        .codex_home(path.clone())
        .fallback_cwd(Some(path))
        .build()
        .await
        .expect("load test config");
    (home, config)
}

#[tokio::test]
async fn bare_antigravity_without_defaults_stays_provider_default() {
    let (_home, config) = test_config().await;

    assert_eq!(
        resolve_effective_selector(&config, "antigravity"),
        "antigravity"
    );
    assert_eq!(resolve_effort(&config, "antigravity", None), None);
}

#[tokio::test]
async fn configured_antigravity_model_promotes_bare_selector() {
    let (_home, mut config) = test_config().await;
    config.agent_selector_overrides.insert(
        "antigravity".to_string(),
        AgentSelectorToml {
            model: Some("gemini-3.1-pro".to_string()),
            effort: Some("high".to_string()),
            ..Default::default()
        },
    );

    assert_eq!(
        resolve_effective_selector(&config, "antigravity"),
        "antigravity-gemini-3.1-pro"
    );
    assert_eq!(
        resolve_effort(&config, "antigravity", None).as_deref(),
        Some("high")
    );
    assert_eq!(
        resolve_effort(&config, "antigravity", Some("low")).as_deref(),
        Some("low")
    );
}

#[tokio::test]
async fn configured_defaults_do_not_replace_explicit_antigravity_model() {
    let (_home, mut config) = test_config().await;
    config.agent_selector_overrides.insert(
        "antigravity".to_string(),
        AgentSelectorToml {
            model: Some("gemini-3.1-pro".to_string()),
            ..Default::default()
        },
    );

    assert_eq!(
        resolve_effective_selector(&config, "antigravity-gemini-3.6-flash-high"),
        "antigravity-gemini-3.6-flash-high"
    );
}

#[tokio::test]
async fn configured_defaults_install_exact_antigravity_arguments() {
    let (_home, mut config) = test_config().await;
    config.agent_selector_overrides.insert(
        "antigravity".to_string(),
        AgentSelectorToml {
            model: Some("gemini-3.1-pro".to_string()),
            effort: Some("high".to_string()),
            ..Default::default()
        },
    );

    install_configured_provider_selectors(&mut config).expect("install configured selector");
    let role = crate::agent::role::resolve_role_config_owned(&config, "antigravity-gemini-3.1-pro")
        .expect("configured selector should resolve");
    let Some(AgentRoleBackendConfig::ExternalCommand(backend)) = role.backend else {
        panic!("configured selector should use external command backend");
    };
    assert!(
        backend
            .args
            .windows(2)
            .any(|args| args == ["--model", "gemini-3.1-pro"])
    );
    assert!(
        backend
            .args
            .windows(2)
            .any(|args| args == ["--effort", "high"])
    );
}

#[tokio::test]
async fn effort_only_default_installs_on_bare_antigravity() {
    let (_home, mut config) = test_config().await;
    config.agent_selector_overrides.insert(
        "antigravity".to_string(),
        AgentSelectorToml {
            effort: Some("high".to_string()),
            ..Default::default()
        },
    );

    install_configured_provider_selectors(&mut config).expect("install configured selector");
    let role = crate::agent::role::resolve_role_config_owned(&config, "antigravity")
        .expect("bare selector should resolve");
    let Some(AgentRoleBackendConfig::ExternalCommand(backend)) = role.backend else {
        panic!("configured selector should use external command backend");
    };
    assert!(
        backend
            .args
            .windows(2)
            .any(|args| args == ["--effort", "high"])
    );
    assert!(!backend.args.iter().any(|arg| arg == "--model"));
}

#[tokio::test]
async fn configured_claude_defaults_replace_external_arguments() {
    let (_home, mut config) = test_config().await;
    config.agent_selector_overrides.insert(
        "claude-sonnet-4.6".to_string(),
        AgentSelectorToml {
            model: Some("claude-opus-4-6".to_string()),
            effort: Some("high".to_string()),
            ..Default::default()
        },
    );

    install_configured_provider_selectors(&mut config).expect("install configured selector");
    let role = crate::agent::role::resolve_role_config_owned(&config, "claude-sonnet-4.6")
        .expect("configured selector should resolve");
    let Some(AgentRoleBackendConfig::ExternalCommand(backend)) = role.backend else {
        panic!("configured selector should use external command backend");
    };
    assert!(
        backend
            .args
            .windows(2)
            .any(|args| args == ["--model", "claude-opus-4-6"])
    );
    assert!(
        backend
            .args
            .windows(2)
            .any(|args| args == ["--effort", "high"])
    );
    assert_eq!(
        backend.args.iter().filter(|arg| *arg == "--model").count(),
        1
    );
    assert_eq!(
        backend.args.iter().filter(|arg| *arg == "--effort").count(),
        1
    );
}

#[tokio::test]
async fn configured_defaults_ignore_native_roles() {
    let (_home, mut config) = test_config().await;
    config.agent_selector_overrides.insert(
        "explorer".to_string(),
        AgentSelectorToml {
            effort: Some("high".to_string()),
            ..Default::default()
        },
    );

    install_configured_provider_selectors(&mut config)
        .expect("native role defaults should not affect external selectors");
    assert!(
        !install_selected_provider_defaults(&mut config, "explorer", Some("high"))
            .expect("native role should be ignored")
    );
    assert!(
        crate::agent::role::resolve_role_config_owned(&config, "explorer")
            .expect("native role should resolve")
            .backend
            .is_none()
    );
}

#[tokio::test]
async fn enabled_model_selector_can_use_disabled_provider_base() {
    let (_home, mut config) = test_config().await;
    config.agent_selector_overrides.insert(
        "antigravity".to_string(),
        AgentSelectorToml {
            enabled: Some(false),
            ..Default::default()
        },
    );
    config.agent_selector_overrides.insert(
        "antigravity-gemini-3.6-flash".to_string(),
        AgentSelectorToml {
            enabled: Some(true),
            effort: Some("high".to_string()),
            ..Default::default()
        },
    );

    install_configured_provider_selectors(&mut config).expect("install enabled model selector");
    assert!(
        crate::agent::role::resolve_role_config_owned(&config, "antigravity-gemini-3.6-flash",)
            .is_some()
    );
    assert!(crate::agent::role::resolve_role_config_owned(&config, "antigravity").is_none());
}
