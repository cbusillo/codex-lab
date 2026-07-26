use codex_protocol::protocol::SessionProvenance;

pub(crate) fn session_provenance_from_agent_env() -> Option<SessionProvenance> {
    session_provenance_from_agent_env_vars(std::env::vars())
}

fn session_provenance_from_agent_env_vars<I, K, V>(vars: I) -> Option<SessionProvenance>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let vars: Vec<(String, String)> = vars
        .into_iter()
        .map(|(key, value)| (key.as_ref().to_string(), value.as_ref().to_string()))
        .collect();

    let has_generic_marker = has_any_env_value(
        &vars,
        &[
            "AGENT_SESSION_ORIGIN",
            "AGENT_SESSION_SOURCE",
            "AGENT_SESSION_REQUEST_ID",
        ],
    );
    let has_legacy_marker = has_any_env_value(
        &vars,
        &[
            "EVERY_CODE_SESSION_ORIGIN",
            "EVERY_CODE_ORIGIN",
            "LAUNCHPLANE_EVERY_CODE_ORIGIN",
            "EVERY_CODE_REQUEST_ID",
        ],
    );
    if !has_generic_marker && !has_legacy_marker {
        return None;
    }

    let origin = first_env_value(
        &vars,
        &[
            "AGENT_SESSION_ORIGIN",
            "EVERY_CODE_SESSION_ORIGIN",
            "EVERY_CODE_ORIGIN",
            "LAUNCHPLANE_EVERY_CODE_ORIGIN",
        ],
    )
    .or_else(|| has_generic_marker.then_some("agent_session".to_string()))
    .or_else(|| has_legacy_marker.then_some("every_code".to_string()));

    let provenance = SessionProvenance {
        request_id: first_env_value(
            &vars,
            &["AGENT_SESSION_REQUEST_ID", "EVERY_CODE_REQUEST_ID"],
        ),
        repository: first_env_value(
            &vars,
            &[
                "AGENT_SESSION_REPOSITORY",
                "AGENT_SESSION_REPO",
                "EVERY_CODE_REPOSITORY",
                "EVERY_CODE_REPO",
            ],
        ),
        issue_number: first_env_value(
            &vars,
            &["AGENT_SESSION_ISSUE_NUMBER", "EVERY_CODE_ISSUE_NUMBER"],
        )
        .and_then(|value| parse_issue_number(&value)),
        issue_url: first_env_value(&vars, &["AGENT_SESSION_ISSUE_URL", "EVERY_CODE_ISSUE_URL"]),
        source: first_env_value(&vars, &["AGENT_SESSION_SOURCE"]),
        origin,
    };

    Some(provenance)
}

fn first_env_value(vars: &[(String, String)], keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| env_value(vars, key))
}

fn has_any_env_value(vars: &[(String, String)], keys: &[&str]) -> bool {
    first_env_value(vars, keys).is_some()
}

fn parse_issue_number(value: &str) -> Option<u64> {
    match value.parse::<u64>() {
        Ok(number) => Some(number),
        Err(err) => {
            tracing::warn!(%value, %err, "Ignoring invalid agent session issue number");
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
#[path = "agent_session_env_tests.rs"]
mod tests;
