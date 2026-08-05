use super::*;
use crate::agent::external_capabilities::ExternalAgentCapabilityFreshness;
use crate::agent::external_capabilities::ExternalAgentCapabilitySource;
use crate::agent::external_capabilities::clear_active_capability_catalog;
use crate::agent::external_capabilities::clear_capability_cache;
use crate::agent::external_capabilities::discovered_antigravity_selectors;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn shell_backend(
    temp_dir: &TempDir,
    name: &str,
    family: &str,
    script: &str,
) -> ExternalCommandAgentBackendConfig {
    let script_path = temp_dir.path().join(name);
    std::fs::write(&script_path, script).expect("write fake external-agent CLI");
    ExternalCommandAgentBackendConfig {
        command: format!("/bin/sh {}", script_path.display()),
        protocol: ExternalCommandProtocol::RawCli,
        launch_family: Some(family.to_string()),
        timeout_ms: 5_000,
        ..Default::default()
    }
}

/// The Antigravity workspace guard runs before the private launch directory is
/// created, so an integration fixture cannot reach it without a session whose
/// `cwd` does not exist.
#[tokio::test]
async fn antigravity_preflight_requires_an_existing_workspace() {
    let temp_dir = TempDir::new().expect("tempdir");
    let missing_workspace = temp_dir.path().join("missing-workspace");
    let backend = ExternalCommandAgentBackendConfig {
        command: "/bin/echo".to_string(),
        protocol: ExternalCommandProtocol::RawCli,
        launch_family: Some("antigravity".to_string()),
        timeout_ms: 5_000,
        ..Default::default()
    };

    let error = preflight_external_agent_backend(
        Some("antigravity"),
        &backend,
        &missing_workspace,
        /*is_read_only*/ true,
    )
    .await
    .expect_err("missing antigravity workspace should fail preflight");

    assert_eq!(
        error,
        ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::LaunchFailed,
            format!(
                "antigravity workspace directory does not exist: {}",
                missing_workspace.display()
            ),
        )
    );
}

/// Preflight probes inherit the parent environment, so the only way to observe
/// the target-dir strip is on the computed backend environment itself.
#[test]
fn preflight_env_strips_cargo_target_dir_overrides() {
    let backend = ExternalCommandAgentBackendConfig {
        env: HashMap::from([
            ("EXTERNAL_AGENT_ENV".to_string(), "configured".to_string()),
            (
                CARGO_TARGET_DIR_ENV_VAR.to_string(),
                "/tmp/shared-target".to_string(),
            ),
            (
                CODEX_LAB_CARGO_TARGET_DIR_ENV_VAR.to_string(),
                "/tmp/explicit-target".to_string(),
            ),
        ]),
        ..Default::default()
    };

    let env = external_agent_backend_env(&backend);

    assert_eq!(
        env,
        HashMap::from([("EXTERNAL_AGENT_ENV".to_string(), "configured".to_string())])
    );
}

/// A hanging provider must surface the probe name and command in the timeout
/// detail; the integration path only ever sees the rendered message.
#[cfg(unix)]
#[tokio::test]
async fn timed_out_preflight_reports_the_probe_and_command() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().expect("tempdir");
    let command_path = temp_dir.path().join("hanging-provider");
    tokio::fs::write(&command_path, "#!/bin/sh\nsleep 30\n")
        .await
        .expect("hanging provider should be written");
    let mut permissions = std::fs::metadata(&command_path)
        .expect("hanging provider metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&command_path, permissions)
        .expect("hanging provider should be executable");

    let error = run_external_agent_preflight_command_with_timeout(
        &ExternalCommandAgentBackendConfig::default(),
        &command_path,
        &[],
        temp_dir.path(),
        &["auth", "status"],
        "authentication",
        ExternalAgentPreflightOutputLimit::Diagnostic,
        Duration::from_millis(200),
    )
    .await
    .expect_err("hanging preflight should time out");

    assert_eq!(
        error,
        ExternalAgentFailureDetail::new(
            ExternalAgentFailureKind::TimedOut,
            format!(
                "timed out while running authentication preflight for `{}`",
                command_path.display()
            ),
        )
    );
}

#[cfg(unix)]
#[tokio::test]
async fn current_claude_capabilities_use_static_models_and_local_flags() {
    clear_capability_cache();
    let temp_dir = TempDir::new().expect("tempdir");
    let backend = shell_backend(
        &temp_dir,
        "fake-claude.sh",
        "claude",
        r#"if [ "$1" = "--version" ]; then
  echo "2.1.220 (Claude Code)"
  exit 0
fi
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  echo '{"loggedIn":true}'
  exit 0
fi
if [ "$1" = "--help" ]; then
  dd if=/dev/zero bs=1024 count=16 2>/dev/null | tr '\\000' x
  echo
  echo '--model <model>'
  echo '--effort <level>'
  exit 0
fi
exit 2
"#,
    );

    let capabilities = discover_external_agent_capabilities(&backend, temp_dir.path()).await;

    assert_eq!(
        capabilities.source,
        ExternalAgentCapabilitySource::StaticCatalog
    );
    assert_eq!(
        capabilities.cli_version.as_deref(),
        Some("2.1.220 (Claude Code)")
    );
    assert!(capabilities.supports_model_selection);
    assert!(capabilities.supports_effort_selection);
    assert!(capabilities.failure.is_none());
    assert!(
        capabilities
            .models
            .iter()
            .any(|model| model.selector == "claude-opus-5")
    );
    assert!(
        capabilities
            .models
            .iter()
            .any(|model| { model.selector == "claude-fable-5" && model.explicit_only })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn current_antigravity_capabilities_are_discovered_and_cached() {
    clear_capability_cache();
    let temp_dir = TempDir::new().expect("tempdir");
    let marker_path = temp_dir.path().join("probe-count");
    let backend = shell_backend(
        &temp_dir,
        "fake-agy.sh",
        "antigravity",
        &format!(
            r#"if [ "$1" = "--version" ]; then
  echo version >> '{}'
  echo "1.1.9"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo models >> '{}'
  echo 'gemini-3.6-flash-high'
  echo 'gemini-3.1-pro-low'
  exit 0
fi
if [ "$1" = "--help" ]; then
  echo help >> '{}'
  echo '--model Model'
  echo '--effort low|medium|high'
  exit 0
fi
exit 2
"#,
            marker_path.display(),
            marker_path.display(),
            marker_path.display()
        ),
    );

    let fresh = discover_external_agent_capabilities(&backend, temp_dir.path()).await;
    let cached = discover_external_agent_capabilities(&backend, temp_dir.path()).await;

    assert_eq!(fresh.source, ExternalAgentCapabilitySource::LocalCli);
    assert_eq!(fresh.freshness, ExternalAgentCapabilityFreshness::Fresh);
    assert_eq!(cached.freshness, ExternalAgentCapabilityFreshness::Cached);
    assert_eq!(
        fresh
            .models
            .iter()
            .map(|model| model.selector.as_str())
            .collect::<Vec<_>>(),
        vec![
            "antigravity-gemini-3.6-flash-high",
            "antigravity-gemini-3.1-pro-low",
        ]
    );
    assert!(fresh.supports_model_selection);
    assert!(fresh.supports_effort_selection);
    assert_eq!(
        std::fs::read_to_string(&marker_path)
            .expect("read capability probe marker")
            .lines()
            .count(),
        3,
        "version, models, and help should each run once across a discovery cache hit"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn antigravity_catalog_is_scoped_and_deterministic() {
    clear_capability_cache();
    clear_active_capability_catalog();
    let temp_dir = TempDir::new().expect("tempdir");
    let first = shell_backend(
        &temp_dir,
        "first-agy.sh",
        "antigravity",
        "case \"$1\" in --version) echo one;; models) printf '%s\\n' z-model a-model a-model;; --help) echo --model;; esac",
    );
    let second = shell_backend(
        &temp_dir,
        "second-agy.sh",
        "antigravity",
        "case \"$1\" in --version) echo two;; models) printf '%s\\n' y-model;; --help) echo --model;; esac",
    );
    let first_caps = discover_external_agent_capabilities(&first, temp_dir.path()).await;
    let second_caps = discover_external_agent_capabilities(&second, temp_dir.path()).await;
    assert_eq!(
        discovered_antigravity_selectors(&first, temp_dir.path()),
        vec![first_caps.models[1].clone(), first_caps.models[0].clone()]
    );
    assert_eq!(
        discovered_antigravity_selectors(&second, temp_dir.path()),
        second_caps.models
    );
}

#[cfg(unix)]
#[tokio::test]
async fn old_antigravity_cli_degrades_to_provider_defaults() {
    clear_capability_cache();
    let temp_dir = TempDir::new().expect("tempdir");
    let backend = shell_backend(
        &temp_dir,
        "old-agy.sh",
        "antigravity",
        r#"if [ "$1" = "--version" ]; then
  echo "0.9.0"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo 'gemini-legacy'
  exit 0
fi
if [ "$1" = "--help" ]; then
  echo 'Usage: agy -p prompt'
  exit 0
fi
exit 2
"#,
    );

    let capabilities = discover_external_agent_capabilities(&backend, temp_dir.path()).await;

    assert!(!capabilities.supports_model_selection);
    assert!(!capabilities.supports_effort_selection);
    assert!(capabilities.failure.is_none());
    preflight_external_agent_backend(
        Some("antigravity"),
        &backend,
        temp_dir.path(),
        /*is_read_only*/ true,
    )
    .await
    .expect("provider-default Antigravity launch should survive an old CLI");
}

#[tokio::test]
async fn missing_cli_capability_report_is_actionable() {
    let temp_dir = TempDir::new().expect("tempdir");
    let backend = ExternalCommandAgentBackendConfig {
        command: "definitely-missing-capability-command".to_string(),
        protocol: ExternalCommandProtocol::RawCli,
        launch_family: Some("claude".to_string()),
        ..Default::default()
    };

    let capabilities = discover_external_agent_capabilities(&backend, temp_dir.path()).await;

    assert_eq!(
        capabilities.failure.as_ref().map(|failure| failure.kind),
        Some(ExternalAgentFailureKind::CommandMissing)
    );
    assert!(
        capabilities
            .failure
            .as_ref()
            .is_some_and(|failure| failure.to_string().contains("Install claude-code"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn signed_out_and_malformed_antigravity_results_are_classified() {
    clear_capability_cache();
    let temp_dir = TempDir::new().expect("tempdir");
    let signed_out = shell_backend(
        &temp_dir,
        "signed-out-agy.sh",
        "antigravity",
        r#"if [ "$1" = "--version" ]; then
  echo "1.1.9"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo 'Authentication required. Please sign in.' >&2
  exit 1
fi
exit 2
"#,
    );
    let signed_out = discover_external_agent_capabilities(&signed_out, temp_dir.path()).await;
    assert_eq!(
        signed_out.failure.as_ref().map(|failure| failure.kind),
        Some(ExternalAgentFailureKind::AuthenticationRequired)
    );

    let malformed = shell_backend(
        &temp_dir,
        "malformed-agy.sh",
        "antigravity",
        r#"if [ "$1" = "--version" ]; then
  echo "1.1.10"
  exit 0
fi
if [ "$1" = "models" ]; then
  echo 'Gemini 3.1 Pro'
  exit 0
fi
if [ "$1" = "--help" ]; then
  echo '--model Model'
  exit 0
fi
exit 2
"#,
    );
    let malformed = discover_external_agent_capabilities(&malformed, temp_dir.path()).await;
    assert_eq!(
        malformed.failure.as_ref().map(|failure| failure.kind),
        Some(ExternalAgentFailureKind::MalformedOutput)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oversized_antigravity_model_output_is_rejected() {
    clear_capability_cache();
    let temp_dir = TempDir::new().expect("tempdir");
    let backend = shell_backend(
        &temp_dir,
        "oversized-agy.sh",
        "antigravity",
        r#"if [ "$1" = "--version" ]; then
  echo "1.1.11"
  exit 0
fi
if [ "$1" = "models" ]; then
  i=0
  while [ "$i" -lt 500 ]; do
    echo "gemini-oversized-$i"
    i=$((i + 1))
  done
  exit 0
fi
if [ "$1" = "--help" ]; then
  echo '--model Model'
  exit 0
fi
exit 2
"#,
    );

    let capabilities = discover_external_agent_capabilities(&backend, temp_dir.path()).await;

    assert_eq!(
        capabilities.failure.as_ref().map(|failure| failure.kind),
        Some(ExternalAgentFailureKind::MalformedOutput)
    );
    assert!(capabilities.models.is_empty());
}
