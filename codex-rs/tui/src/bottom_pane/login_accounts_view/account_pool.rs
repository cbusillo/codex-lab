use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::StoredAccount;

use super::AccountRow;

pub(super) const CURRENT_AUTH_ACCOUNT_ID: &str = "current-auth-login";
pub(super) const CURRENT_ONLY_POOL_NOTICE: &str = "This credential mode keeps one current login. It is not enrolled in the account-switching pool; signing in again replaces it.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AccountPoolBehavior {
    Pooled,
    CurrentOnly,
}

impl AccountPoolBehavior {
    pub(super) fn for_store_mode(auth_credentials_store_mode: AuthCredentialsStoreMode) -> Self {
        match auth_credentials_store_mode {
            AuthCredentialsStoreMode::File => Self::Pooled,
            AuthCredentialsStoreMode::Keyring
            | AuthCredentialsStoreMode::Auto
            | AuthCredentialsStoreMode::Ephemeral => Self::CurrentOnly,
        }
    }

    pub(super) fn supports_pooling(self) -> bool {
        self == Self::Pooled
    }

    pub(super) fn account_action_label(self) -> &'static str {
        match self {
            Self::Pooled => "Add account...",
            Self::CurrentOnly => "Replace current login...",
        }
    }

    pub(super) fn add_view_title(self) -> &'static str {
        match self {
            Self::Pooled => " Add Account ",
            Self::CurrentOnly => " Replace Login ",
        }
    }
}

pub(super) fn current_auth_account_row(
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> AccountRow {
    let mode = auth.resolved_mode();
    let label = auth.account_email().unwrap_or_else(|| match mode {
        AuthMode::ApiKey => "API key".to_string(),
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => "ChatGPT".to_string(),
        AuthMode::AgentIdentity => "Agent identity".to_string(),
        AuthMode::PersonalAccessToken => "Personal access token".to_string(),
    });
    let storage = match auth_credentials_store_mode {
        AuthCredentialsStoreMode::Keyring => "keyring",
        AuthCredentialsStoreMode::Auto => "auto storage",
        AuthCredentialsStoreMode::Ephemeral => "in-memory credential storage",
        AuthCredentialsStoreMode::File => "file storage",
    };
    AccountRow {
        id: CURRENT_AUTH_ACCOUNT_ID.to_string(),
        label,
        detail: Some(format!("{storage} - not in switching pool")),
        mode,
        is_active: true,
        is_pooled: false,
    }
}

pub(super) fn account_matches_auth(account: &StoredAccount, auth: &AuthDotJson) -> bool {
    match auth.resolved_mode() {
        AuthMode::ApiKey => {
            account.mode == AuthMode::ApiKey && account.openai_api_key == auth.openai_api_key
        }
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => {
            if !matches!(
                account.mode,
                AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens
            ) {
                return false;
            }
            let Some(account_tokens) = account.tokens.as_ref() else {
                return false;
            };
            let Some(auth_tokens) = auth.tokens.as_ref() else {
                return false;
            };
            let account_id = account_tokens
                .account_id
                .as_deref()
                .or(account_tokens.id_token.chatgpt_account_id.as_deref());
            let auth_account_id = auth_tokens
                .account_id
                .as_deref()
                .or(auth_tokens.id_token.chatgpt_account_id.as_deref());
            account_id.is_some() && account_id == auth_account_id
        }
        AuthMode::AgentIdentity | AuthMode::PersonalAccessToken => false,
    }
}
