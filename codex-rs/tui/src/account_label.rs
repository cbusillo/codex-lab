use codex_login::StoredAccount;
use codex_protocol::auth::AuthMode;

const KEY_SUFFIX_LEN: usize = 8;

/// Returns a user-facing label for a stored account.
pub(crate) fn account_display_label(account: &StoredAccount) -> String {
    if let Some(label) = account.label.as_ref() {
        let trimmed = label.trim();
        if !trimmed.is_empty() {
            match account.mode {
                AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => {
                    let default_email = account
                        .tokens
                        .as_ref()
                        .and_then(|tokens| tokens.id_token.email.as_deref());
                    if !default_email.is_some_and(|email| trimmed.eq_ignore_ascii_case(email)) {
                        return trimmed.to_string();
                    }
                }
                AuthMode::ApiKey
                | AuthMode::Headers
                | AuthMode::AgentIdentity
                | AuthMode::PersonalAccessToken
                | AuthMode::BedrockApiKey => {
                    return trimmed.to_string();
                }
            }
        }
    }

    match account.mode {
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => account
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.id_token.email.clone())
            .map(|email| format!("ChatGPT ({email})"))
            .unwrap_or_else(|| "ChatGPT".to_string()),
        AuthMode::ApiKey => account
            .openai_api_key
            .as_ref()
            .map(|key| format!("API key (...{})", key_suffix(key)))
            .unwrap_or_else(|| "API key".to_string()),
        AuthMode::AgentIdentity => "Agent identity".to_string(),
        AuthMode::PersonalAccessToken => "Personal access token".to_string(),
        AuthMode::Headers => "Request headers".to_string(),
        AuthMode::BedrockApiKey => "Bedrock API key".to_string(),
    }
}

/// Returns the fixed-length suffix used when displaying sensitive tokens.
pub(crate) fn key_suffix(text: &str) -> String {
    let char_count = text.chars().count();
    text.chars()
        .skip(char_count.saturating_sub(KEY_SUFFIX_LEN))
        .collect()
}

/// Returns an ordering priority for stored accounts.
#[cfg(test)]
pub(crate) fn account_mode_priority(mode: AuthMode) -> u8 {
    match mode {
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => 0,
        AuthMode::ApiKey => 1,
        AuthMode::AgentIdentity => 2,
        AuthMode::PersonalAccessToken => 3,
        AuthMode::Headers => 4,
        AuthMode::BedrockApiKey => 5,
    }
}

#[cfg(test)]
#[path = "account_label_tests.rs"]
mod tests;
