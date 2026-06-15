use codex_protocol::protocol::SessionSource;

pub(crate) fn session_source_from_agent_env() -> Option<SessionSource> {
    session_source_from_agent_env_vars(std::env::vars())
}

pub(crate) fn startup_session_source() -> SessionSource {
    session_source_from_agent_env().unwrap_or(SessionSource::Cli)
}

fn session_source_from_agent_env_vars<I, K, V>(vars: I) -> Option<SessionSource>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let vars: Vec<(String, String)> = vars
        .into_iter()
        .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
        .collect();

    let source = env_value(&vars, "AGENT_SESSION_ORIGIN")
        .or_else(|| env_value(&vars, "AGENT_SESSION_SOURCE"))
        .or_else(|| {
            // Request ids infer the source until structured session provenance
            // has a storage/API path in Codex Lab.
            env_value(&vars, "AGENT_SESSION_REQUEST_ID").map(|_| "agent_session".to_string())
        })
        .or_else(|| env_value(&vars, "EVERY_CODE_SESSION_ORIGIN"))
        .or_else(|| env_value(&vars, "EVERY_CODE_ORIGIN"))
        .or_else(|| env_value(&vars, "LAUNCHPLANE_EVERY_CODE_ORIGIN"))
        .or_else(|| {
            // Preserve the legacy inference behavior without pretending the
            // request id itself is stored as structured provenance here.
            env_value(&vars, "EVERY_CODE_REQUEST_ID").map(|_| "every_code".to_string())
        })?;

    match SessionSource::from_startup_arg(&source) {
        Ok(source) => Some(source),
        Err(err) => {
            tracing::warn!(%source, %err, "Ignoring invalid agent session source from environment");
            None
        }
    }
}

fn env_value(vars: &[(String, String)], key: &str) -> Option<String> {
    vars.iter()
        .find(|(candidate_key, _)| candidate_key == key)
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_from(vars: &[(&str, &str)]) -> Option<SessionSource> {
        session_source_from_agent_env_vars(vars.iter().copied())
    }

    fn startup_source_from(vars: &[(&str, &str)]) -> SessionSource {
        session_source_from_agent_env_vars(vars.iter().copied()).unwrap_or(SessionSource::Cli)
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
            SessionSource::Custom("launchplane".to_string())
        );
    }

    #[test]
    fn generic_origin_wins_over_legacy_origin() {
        assert_eq!(
            source_from(&[
                ("AGENT_SESSION_ORIGIN", "launchplane"),
                ("EVERY_CODE_SESSION_ORIGIN", "every_code"),
            ]),
            Some(SessionSource::Custom("launchplane".to_string()))
        );
    }

    #[test]
    fn generic_origin_wins_over_generic_source() {
        assert_eq!(
            source_from(&[
                ("AGENT_SESSION_ORIGIN", "launchplane"),
                ("AGENT_SESSION_SOURCE", "agent-session"),
            ]),
            Some(SessionSource::Custom("launchplane".to_string()))
        );
    }

    #[test]
    fn canonical_source_values_map_to_canonical_variants() {
        assert_eq!(
            source_from(&[("AGENT_SESSION_ORIGIN", "cli")]),
            Some(SessionSource::Cli)
        );
        assert_eq!(
            source_from(&[("AGENT_SESSION_ORIGIN", "vscode")]),
            Some(SessionSource::VSCode)
        );
    }

    #[test]
    fn generic_source_is_used_when_origin_is_missing() {
        assert_eq!(
            source_from(&[("AGENT_SESSION_SOURCE", "agent-session")]),
            Some(SessionSource::Custom("agent-session".to_string()))
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
}
