
use super::*;

#[test]
fn classifies_provider_failure_text() {
    assert_eq!(
        classify_provider_failure_text("HTTP 429: quota exceeded"),
        ExternalAgentFailureKind::QuotaOrRateLimited
    );
    assert_eq!(
        classify_provider_failure_text("Authentication required. Please sign in."),
        ExternalAgentFailureKind::AuthenticationRequired
    );
    assert_eq!(
        classify_provider_failure_text("unknown option --print"),
        ExternalAgentFailureKind::UnsupportedMode
    );
    assert_eq!(
        classify_provider_failure_text("provider exited with status 1"),
        ExternalAgentFailureKind::ProviderFailed
    );
}

#[test]
fn provenance_redacts_command_paths() {
    let provenance = ExternalAgentProviderProvenance::new(
        Some("claude-sonnet-4.6"),
        &ExternalCommandAgentBackendConfig {
            command: "/private/tools/claude --flag".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            launch_family: Some("claude".to_string()),
            ..Default::default()
        },
        Path::new("/tmp/workspace"),
        true,
        Some("2.1.212".to_string()),
    );

    assert_eq!(provenance.command, "claude");
    assert_eq!(provenance.mode, ExternalAgentLaunchMode::ReadOnly);
}
