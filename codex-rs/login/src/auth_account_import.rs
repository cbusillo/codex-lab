use crate::auth::AuthDotJson;
use crate::auth::load_auth_dot_json;
use crate::auth_accounts::StoredAccount;
use crate::auth_accounts::insert_api_key_account_if_missing;
use crate::auth_accounts::insert_chatgpt_account_if_missing;
use crate::auth_profiles::list_auth_profiles;
use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use std::io;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthAccountImportSource {
    Default,
    Profile { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthAccountImportSkipReason {
    MissingAuth,
    ExistingAccount,
    UnsupportedMode,
    InvalidAuth,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImportedAuthAccount {
    pub source: AuthAccountImportSource,
    pub account: StoredAccount,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedAuthAccountImport {
    pub source: AuthAccountImportSource,
    pub reason: AuthAccountImportSkipReason,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuthAccountImportReport {
    pub imported: Vec<ImportedAuthAccount>,
    pub skipped: Vec<SkippedAuthAccountImport>,
}

struct AuthImportCandidate {
    source: AuthAccountImportSource,
    home: PathBuf,
}

pub fn import_auth_accounts_from_auth_homes(
    codex_home: &Path,
) -> io::Result<AuthAccountImportReport> {
    let mut report = AuthAccountImportReport::default();

    for candidate in import_candidates(codex_home)? {
        import_auth_home(codex_home, candidate, &mut report)?;
    }

    Ok(report)
}

fn import_candidates(codex_home: &Path) -> io::Result<Vec<AuthImportCandidate>> {
    let mut candidates = vec![AuthImportCandidate {
        source: AuthAccountImportSource::Default,
        home: codex_home.to_path_buf(),
    }];

    for profile in list_auth_profiles(codex_home)? {
        candidates.push(AuthImportCandidate {
            source: AuthAccountImportSource::Profile { name: profile.name },
            home: profile.home,
        });
    }

    Ok(candidates)
}

fn import_auth_home(
    codex_home: &Path,
    candidate: AuthImportCandidate,
    report: &mut AuthAccountImportReport,
) -> io::Result<()> {
    let auth = match load_auth_dot_json(&candidate.home, AuthCredentialsStoreMode::File) {
        Ok(Some(auth)) => auth,
        Ok(None) => {
            push_skip(
                report,
                candidate.source,
                AuthAccountImportSkipReason::MissingAuth,
            );
            return Ok(());
        }
        Err(err) if is_invalid_auth_error(&err) => {
            push_skip(
                report,
                candidate.source,
                AuthAccountImportSkipReason::InvalidAuth,
            );
            return Ok(());
        }
        Err(err) => return Err(err),
    };

    import_auth_payload(codex_home, candidate.source, auth, report)
}

fn import_auth_payload(
    codex_home: &Path,
    source: AuthAccountImportSource,
    auth: AuthDotJson,
    report: &mut AuthAccountImportReport,
) -> io::Result<()> {
    let imported = match auth.resolved_mode() {
        AuthMode::ApiKey => {
            let Some(api_key) = auth.openai_api_key else {
                push_skip(report, source, AuthAccountImportSkipReason::InvalidAuth);
                return Ok(());
            };
            insert_api_key_account_if_missing(codex_home, api_key, /*label*/ None)?
        }
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => {
            let Some(tokens) = auth.tokens else {
                push_skip(report, source, AuthAccountImportSkipReason::InvalidAuth);
                return Ok(());
            };
            let last_refresh = auth.last_refresh.unwrap_or_else(chrono::Utc::now);
            insert_chatgpt_account_if_missing(
                codex_home,
                tokens.clone(),
                last_refresh,
                tokens.id_token.email,
            )?
        }
        AuthMode::AgentIdentity | AuthMode::PersonalAccessToken => {
            push_skip(report, source, AuthAccountImportSkipReason::UnsupportedMode);
            return Ok(());
        }
    };

    match imported {
        Some(account) => report
            .imported
            .push(ImportedAuthAccount { source, account }),
        None => push_skip(report, source, AuthAccountImportSkipReason::ExistingAccount),
    }

    Ok(())
}

fn is_invalid_auth_error(err: &io::Error) -> bool {
    matches!(err.kind(), io::ErrorKind::InvalidData)
}

fn push_skip(
    report: &mut AuthAccountImportReport,
    source: AuthAccountImportSource,
    reason: AuthAccountImportSkipReason,
) {
    report
        .skipped
        .push(SkippedAuthAccountImport { source, reason });
}

#[cfg(test)]
#[path = "auth_account_import_tests.rs"]
mod tests;
