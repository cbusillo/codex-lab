use super::*;

fn source_from(vars: &[(&str, &str)]) -> Option<SessionSource> {
    session_source_from_agent_env_vars(vars.iter().copied())
}

fn startup_source_from(vars: &[(&str, &str)]) -> SessionSource {
    session_source_from_agent_env_vars(vars.iter().copied()).unwrap_or(SessionSource::Cli)
}

fn provenance_from(vars: &[(&str, &str)]) -> Option<SessionProvenance> {
    session_provenance_from_agent_env_vars(vars.iter().copied())
}

#[test]
fn returns_none_without_agent_session_env() {
    assert_eq!(source_from(&[]), None);
}

#[test]
fn startup_source_defaults_to_cli_without_agent_session_env() {
    assert_eq!(startup_source_from(&[]), SessionSource::Cli);
}

#[test]
fn startup_source_uses_agent_session_env() {
    assert_eq!(
        startup_source_from(&[("AGENT_SESSION_ORIGIN", "launchplane")]),
        SessionSource::Custom("agent_session".to_string())
    );
}

#[test]
fn generic_origin_wins_over_legacy_origin() {
    assert_eq!(
        source_from(&[
            ("AGENT_SESSION_ORIGIN", "launchplane"),
            ("EVERY_CODE_SESSION_ORIGIN", "every_code"),
        ]),
        Some(SessionSource::Custom("agent_session".to_string()))
    );
}

#[test]
fn generic_origin_wins_over_generic_source() {
    assert_eq!(
        source_from(&[
            ("AGENT_SESSION_ORIGIN", "launchplane"),
            ("AGENT_SESSION_SOURCE", "agent-session"),
        ]),
        Some(SessionSource::Custom("agent_session".to_string()))
    );
}

#[test]
fn product_like_origin_values_do_not_drive_runtime_source() {
    assert_eq!(
        source_from(&[("AGENT_SESSION_ORIGIN", "atlas")]),
        Some(SessionSource::Custom("agent_session".to_string()))
    );
    assert_eq!(
        source_from(&[("AGENT_SESSION_SOURCE", "chatgpt")]),
        Some(SessionSource::Custom("agent_session".to_string()))
    );
}

#[test]
fn generic_source_is_used_when_origin_is_missing() {
    assert_eq!(
        source_from(&[("AGENT_SESSION_SOURCE", "agent-session")]),
        Some(SessionSource::Custom("agent_session".to_string()))
    );
}

#[test]
fn generic_request_id_implies_agent_session_source() {
    assert_eq!(
        source_from(&[("AGENT_SESSION_REQUEST_ID", "agent-session-123")]),
        Some(SessionSource::Custom("agent_session".to_string()))
    );
}

#[test]
fn legacy_session_origin_still_works() {
    assert_eq!(
        source_from(&[("EVERY_CODE_SESSION_ORIGIN", "every_code")]),
        Some(SessionSource::Custom("every_code".to_string()))
    );
}

#[test]
fn legacy_request_id_still_implies_every_code_source() {
    assert_eq!(
        source_from(&[("EVERY_CODE_REQUEST_ID", "every-code-cbusillo-code-123")]),
        Some(SessionSource::Custom("every_code".to_string()))
    );
}

#[test]
fn empty_values_are_ignored() {
    assert_eq!(
        source_from(&[
            ("AGENT_SESSION_ORIGIN", "   "),
            ("EVERY_CODE_REQUEST_ID", "every-code-cbusillo-code-123"),
        ]),
        Some(SessionSource::Custom("every_code".to_string()))
    );
}

#[test]
fn provenance_uses_generic_agent_session_contract() {
    assert_eq!(
        provenance_from(&[
            ("AGENT_SESSION_REQUEST_ID", "agent-session-123"),
            ("AGENT_SESSION_REPOSITORY", "cbusillo/codex-lab"),
            ("AGENT_SESSION_ISSUE_NUMBER", "48"),
            (
                "AGENT_SESSION_ISSUE_URL",
                "https://github.com/cbusillo/codex-lab/issues/48"
            ),
            ("AGENT_SESSION_SOURCE", "agent-session"),
            ("AGENT_SESSION_ORIGIN", "launchplane"),
        ]),
        Some(SessionProvenance {
            request_id: Some("agent-session-123".to_string()),
            repository: Some("cbusillo/codex-lab".to_string()),
            issue_number: Some(48),
            issue_url: Some("https://github.com/cbusillo/codex-lab/issues/48".to_string()),
            source: Some("agent-session".to_string()),
            origin: Some("launchplane".to_string()),
        })
    );
}

#[test]
fn provenance_keeps_legacy_every_code_contract() {
    assert_eq!(
        provenance_from(&[
            ("EVERY_CODE_REQUEST_ID", "every-code-123"),
            ("EVERY_CODE_REPOSITORY", "cbusillo/code"),
            ("EVERY_CODE_ISSUE_NUMBER", "58"),
            (
                "EVERY_CODE_ISSUE_URL",
                "https://github.com/cbusillo/code/issues/58"
            ),
            ("EVERY_CODE_SESSION_ORIGIN", "every_code"),
        ]),
        Some(SessionProvenance {
            request_id: Some("every-code-123".to_string()),
            repository: Some("cbusillo/code".to_string()),
            issue_number: Some(58),
            issue_url: Some("https://github.com/cbusillo/code/issues/58".to_string()),
            source: None,
            origin: Some("every_code".to_string()),
        })
    );
}

#[test]
fn provenance_generic_values_win_over_legacy_values() {
    assert_eq!(
        provenance_from(&[
            ("AGENT_SESSION_REQUEST_ID", "agent-session-123"),
            ("EVERY_CODE_REQUEST_ID", "every-code-123"),
            ("AGENT_SESSION_REPOSITORY", "cbusillo/codex-lab"),
            ("EVERY_CODE_REPOSITORY", "cbusillo/code"),
            ("AGENT_SESSION_ORIGIN", "launchplane"),
            ("EVERY_CODE_SESSION_ORIGIN", "every_code"),
        ]),
        Some(SessionProvenance {
            request_id: Some("agent-session-123".to_string()),
            repository: Some("cbusillo/codex-lab".to_string()),
            issue_number: None,
            issue_url: None,
            source: None,
            origin: Some("launchplane".to_string()),
        })
    );
}

#[test]
fn provenance_ignores_empty_values_and_bad_issue_number() {
    assert_eq!(
        provenance_from(&[
            ("AGENT_SESSION_ORIGIN", "launchplane"),
            ("AGENT_SESSION_REPOSITORY", "cbusillo/codex-lab"),
            ("AGENT_SESSION_ISSUE_NUMBER", "not-a-number"),
        ]),
        Some(SessionProvenance {
            request_id: None,
            repository: Some("cbusillo/codex-lab".to_string()),
            issue_number: None,
            issue_url: None,
            source: None,
            origin: Some("launchplane".to_string()),
        })
    );
}

#[test]
fn provenance_infers_origin_from_generic_request_id() {
    assert_eq!(
        provenance_from(&[("AGENT_SESSION_REQUEST_ID", "agent-session-123")]),
        Some(SessionProvenance {
            request_id: Some("agent-session-123".to_string()),
            repository: None,
            issue_number: None,
            issue_url: None,
            source: None,
            origin: Some("agent_session".to_string()),
        })
    );
}

#[test]
fn provenance_infers_origin_from_legacy_request_id() {
    assert_eq!(
        provenance_from(&[("EVERY_CODE_REQUEST_ID", "every-code-123")]),
        Some(SessionProvenance {
            request_id: Some("every-code-123".to_string()),
            repository: None,
            issue_number: None,
            issue_url: None,
            source: None,
            origin: Some("every_code".to_string()),
        })
    );
}

#[test]
fn provenance_ignores_metadata_without_session_marker() {
    assert_eq!(
        provenance_from(&[
            ("AGENT_SESSION_REPOSITORY", "cbusillo/codex-lab"),
            ("AGENT_SESSION_ISSUE_NUMBER", "126"),
            (
                "AGENT_SESSION_ISSUE_URL",
                "https://github.com/cbusillo/codex-lab/issues/126"
            ),
        ]),
        None
    );
}

#[test]
fn provenance_ignores_metadata_with_empty_session_marker() {
    assert_eq!(
        provenance_from(&[
            ("AGENT_SESSION_REQUEST_ID", "   "),
            ("AGENT_SESSION_REPOSITORY", "cbusillo/codex-lab"),
            ("AGENT_SESSION_ISSUE_NUMBER", "126"),
        ]),
        None
    );
}

#[test]
fn provenance_returns_none_without_structured_env() {
    assert_eq!(provenance_from(&[]), None);
}
