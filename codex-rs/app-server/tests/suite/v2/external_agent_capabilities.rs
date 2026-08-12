use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ExternalAgentAuthState;
use codex_app_server_protocol::ExternalAgentCapabilitiesReadResponse;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshCancelResponse;
use codex_app_server_protocol::ExternalAgentCapabilitiesRefreshStatus;
use codex_app_server_protocol::ExternalAgentCapabilitiesUpdatedNotification;
use codex_app_server_protocol::ExternalAgentCapabilitySource;
use codex_app_server_protocol::ExternalAgentInstallState;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn capabilities_read_is_cache_only_and_reports_effective_origins() -> Result<()> {
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        r#"
[agents.selectors.antigravity]
enabled = false
model = "gemini-3-pro"
effort = "high"
"#,
    )?;
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .build_initialized()
        .await?;

    let request_id = app_server
        .send_raw_request(
            "externalAgentCapability/read",
            Some(serde_json::json!({
                "cwd": codex_home.path(),
            })),
        )
        .await?;
    let response: ExternalAgentCapabilitiesReadResponse =
        timeout(RESPONSE_TIMEOUT, app_server.read_response(request_id)).await??;

    assert_eq!(response.providers.len(), 4);
    assert!(response.refresh.is_none());
    assert!(response.providers.iter().all(|provider| {
        provider.source == ExternalAgentCapabilitySource::CacheMiss
            && provider.install == ExternalAgentInstallState::Unknown
            && provider.auth == ExternalAgentAuthState::Unknown
    }));
    let antigravity = response
        .providers
        .iter()
        .find(|provider| provider.family == "antigravity")
        .expect("antigravity provider");
    let selector = antigravity
        .selectors
        .iter()
        .find(|selector| selector.selector == "antigravity")
        .expect("antigravity selector");
    assert!(!selector.enabled);
    assert_eq!(selector.default_model.as_deref(), Some("gemini-3-pro"));
    assert_eq!(selector.default_effort.as_deref(), Some("high"));
    assert!(
        selector
            .enabled_origin
            .as_ref()
            .is_some_and(|origin| !origin.read_only)
    );
    let claude = response
        .providers
        .iter()
        .find(|provider| provider.family == "claude")
        .expect("claude provider");
    assert!(
        claude
            .selectors
            .iter()
            .any(|selector| { selector.selector == "claude-fable-5" && selector.explicit_only })
    );

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn capabilities_refresh_notifies_with_discovered_antigravity_models() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let codex_home = TempDir::new()?;
    let bin_dir = TempDir::new()?;
    let agy_path = bin_dir.path().join("agy");
    std::fs::write(
        &agy_path,
        r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    --version)
      echo "agy 1.2.3"
      exit 0
      ;;
    models)
      printf '%s\n' 'Available models' '| gemini-3-pro | Gemini 3 Pro |' '- gemini-3-flash (default)'
      exit 0
      ;;
    --help)
      printf '%s\n' '--model MODEL' '--effort EFFORT'
      exit 0
      ;;
  esac
done
exit 0
"#,
    )?;
    let mut permissions = std::fs::metadata(&agy_path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&agy_path, permissions)?;
    let path = std::env::var("PATH")?;
    let augmented_path = format!("{}:{path}", bin_dir.path().display());
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("PATH", Some(augmented_path.as_str()))])
        .build_initialized()
        .await?;

    let request_id = app_server
        .send_raw_request(
            "externalAgentCapability/read",
            Some(serde_json::json!({
                "cwd": codex_home.path(),
                "families": ["antigravity"],
                "refresh": { "refreshId": "refresh-antigravity" },
            })),
        )
        .await?;
    let response: ExternalAgentCapabilitiesReadResponse =
        timeout(RESPONSE_TIMEOUT, app_server.read_response(request_id)).await??;
    assert_eq!(
        response
            .refresh
            .as_ref()
            .map(|refresh| refresh.refresh_id.as_str()),
        Some("refresh-antigravity")
    );

    let notification: ExternalAgentCapabilitiesUpdatedNotification = timeout(
        Duration::from_secs(10),
        app_server.read_notification("externalAgentCapability/updated"),
    )
    .await??;
    assert_eq!(
        notification.status,
        ExternalAgentCapabilitiesRefreshStatus::Completed
    );
    let provider = notification.providers.first().expect("provider");
    assert_eq!(provider.install, ExternalAgentInstallState::Installed);
    assert_eq!(provider.auth, ExternalAgentAuthState::Authenticated);
    assert!(provider.selectors.iter().any(|selector| {
        selector.selector == "antigravity-gemini-3-pro" && selector.discovered && selector.enabled
    }));
    assert!(provider.selectors.iter().any(|selector| {
        selector.selector == "antigravity-gemini-3-flash" && selector.discovered && selector.enabled
    }));

    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn capabilities_refresh_cancel_stops_the_background_probe() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let codex_home = TempDir::new()?;
    let bin_dir = TempDir::new()?;
    let completion_marker = bin_dir.path().join("completed");
    let agy_path = bin_dir.path().join("agy");
    std::fs::write(
        &agy_path,
        format!(
            r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    --version)
      echo "agy 1.2.3"
      exit 0
      ;;
    models)
      sleep 30
      touch '{}'
      echo 'gemini-3-pro'
      exit 0
      ;;
    --help)
      printf '%s\n' '--model MODEL' '--effort EFFORT'
      exit 0
      ;;
  esac
done
exit 0
"#,
            completion_marker.display()
        ),
    )?;
    let mut permissions = std::fs::metadata(&agy_path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&agy_path, permissions)?;
    let path = std::env::var("PATH")?;
    let augmented_path = format!("{}:{path}", bin_dir.path().display());
    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[("PATH", Some(augmented_path.as_str()))])
        .build_initialized()
        .await?;

    let request_id = app_server
        .send_raw_request(
            "externalAgentCapability/read",
            Some(serde_json::json!({
                "cwd": codex_home.path(),
                "families": ["antigravity"],
                "refresh": { "refreshId": "refresh-cancel" },
            })),
        )
        .await?;
    let response: ExternalAgentCapabilitiesReadResponse =
        timeout(RESPONSE_TIMEOUT, app_server.read_response(request_id)).await??;
    assert!(response.refresh.is_some());

    let cancel_id = app_server
        .send_raw_request(
            "externalAgentCapability/refresh/cancel",
            Some(serde_json::json!({ "refreshId": "refresh-cancel" })),
        )
        .await?;
    let cancelled: ExternalAgentCapabilitiesRefreshCancelResponse =
        timeout(RESPONSE_TIMEOUT, app_server.read_response(cancel_id)).await??;
    assert!(cancelled.cancelled);
    let notification: ExternalAgentCapabilitiesUpdatedNotification = timeout(
        RESPONSE_TIMEOUT,
        app_server.read_notification("externalAgentCapability/updated"),
    )
    .await??;
    assert_eq!(
        notification.status,
        ExternalAgentCapabilitiesRefreshStatus::Cancelled
    );
    assert!(!completion_marker.exists());

    Ok(())
}
