use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

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
