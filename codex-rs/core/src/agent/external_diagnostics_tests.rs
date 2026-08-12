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
        classify_provider_failure_text(
            "a tool required the command permission that headless mode cannot prompt for, so it was auto-denied"
        ),
        ExternalAgentFailureKind::PermissionDenied
    );
    assert_eq!(
        classify_provider_failure_text("sh: ./tool: Permission denied"),
        ExternalAgentFailureKind::ProviderFailed
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
        /*is_read_only*/ true,
        Some("2.1.212".to_string()),
    );

    assert_eq!(provenance.command, "claude");
    assert_eq!(provenance.mode, ExternalAgentLaunchMode::ReadOnly);
}

#[test]
fn provenance_reports_the_last_explicit_model_and_effort() {
    let provenance = ExternalAgentProviderProvenance::new(
        Some("antigravity-gemini-new"),
        &ExternalCommandAgentBackendConfig {
            command: "agy".to_string(),
            protocol: ExternalCommandProtocol::RawCli,
            args: [
                "--model=gemini-old",
                "--effort",
                "low",
                "--model",
                "gemini-new",
                "--effort=high",
            ]
            .map(str::to_string)
            .to_vec(),
            launch_family: Some("antigravity".to_string()),
            ..Default::default()
        },
        Path::new("/tmp/workspace"),
        /*is_read_only*/ false,
        Some("1.1.10".to_string()),
    );

    assert_eq!(provenance.model.as_deref(), Some("gemini-new"));
    assert_eq!(provenance.effort.as_deref(), Some("high"));
}
