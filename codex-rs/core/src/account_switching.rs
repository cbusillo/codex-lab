use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::path::Path;

use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::StoredAccount;

use crate::account_usage;
use codex_protocol::protocol::RateLimitReachedType;

#[derive(Debug, Default)]
pub struct RateLimitSwitchState {
    tried_accounts: HashSet<String>,
    blocked_until: HashMap<String, DateTime<Utc>>,
}

impl RateLimitSwitchState {
    pub fn mark_limited(&mut self, account_id: &str, blocked_until: Option<DateTime<Utc>>) {
        self.tried_accounts.insert(account_id.to_string());
        if let Some(until) = blocked_until {
            self.blocked_until
                .entry(account_id.to_string())
                .and_modify(|existing| {
                    if until > *existing {
                        *existing = until;
                    }
                })
                .or_insert(until);
        }
    }

    fn has_tried(&self, account_id: &str) -> bool {
        self.tried_accounts.contains(account_id)
    }

    fn blocked_until(&self, account_id: &str) -> Option<DateTime<Utc>> {
        self.blocked_until.get(account_id).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CandidateScore {
    reset_at: Option<DateTime<Utc>>,
    used_percent: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AccountSwitchOutcome {
    Switched(StoredAccount),
    NoCandidate,
}

fn account_has_credentials(account: &StoredAccount) -> bool {
    match account.mode {
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => account.tokens.is_some(),
        AuthMode::ApiKey => account.openai_api_key.is_some(),
        AuthMode::PersonalAccessToken | AuthMode::AgentIdentity => false,
    }
}

fn usage_limit_reached_type(reached: RateLimitReachedType) -> bool {
    matches!(
        reached,
        RateLimitReachedType::RateLimitReached
            | RateLimitReachedType::WorkspaceOwnerCreditsDepleted
            | RateLimitReachedType::WorkspaceMemberCreditsDepleted
            | RateLimitReachedType::WorkspaceOwnerUsageLimitReached
            | RateLimitReachedType::WorkspaceMemberUsageLimitReached
    )
}

fn usage_reset_blocked_until(
    snapshot: &account_usage::StoredRateLimitSnapshot,
) -> Option<DateTime<Utc>> {
    let reached = snapshot
        .snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.rate_limit_reached_type)
        .is_some_and(usage_limit_reached_type);
    let (primary_exhausted, secondary_exhausted) = snapshot
        .snapshot
        .as_ref()
        .map(|snapshot| {
            (
                snapshot
                    .primary
                    .as_ref()
                    .is_some_and(|window| window.used_percent >= 100.0),
                snapshot
                    .secondary
                    .as_ref()
                    .is_some_and(|window| window.used_percent >= 100.0),
            )
        })
        .unwrap_or_default();
    let hinted_limit = snapshot.last_usage_limit_hit_at.is_some();

    if reached || primary_exhausted || secondary_exhausted || hinted_limit {
        let primary_reset = (reached || primary_exhausted || hinted_limit)
            .then_some(snapshot.primary_next_reset_at)
            .flatten();
        let secondary_reset = (reached || secondary_exhausted || hinted_limit)
            .then_some(snapshot.secondary_next_reset_at)
            .flatten();
        return primary_reset
            .into_iter()
            .chain(secondary_reset)
            .max()
            .or(snapshot.last_usage_limit_hit_at);
    }

    None
}

fn usage_used_percent(snapshot: &account_usage::StoredRateLimitSnapshot) -> Option<f64> {
    let snapshot = snapshot.snapshot.as_ref()?;
    let primary = snapshot
        .primary
        .as_ref()
        .map(|window| window.used_percent)
        .unwrap_or_default();
    let secondary = snapshot
        .secondary
        .as_ref()
        .map(|window| window.used_percent)
        .unwrap_or_default();
    let used = primary.max(secondary);
    used.is_finite().then_some(used)
}

fn usage_preferred_reset_at(
    snapshot: &account_usage::StoredRateLimitSnapshot,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let secondary_reset = snapshot
        .secondary_next_reset_at
        .filter(|reset_at| *reset_at > now);
    let primary_reset = snapshot
        .primary_next_reset_at
        .filter(|reset_at| *reset_at > now);
    secondary_reset.or(primary_reset)
}

fn candidate_score(
    snapshot_map: &HashMap<String, account_usage::StoredRateLimitSnapshot>,
    account_id: &str,
    now: DateTime<Utc>,
) -> CandidateScore {
    let snapshot = snapshot_map.get(account_id);
    CandidateScore {
        reset_at: snapshot.and_then(|snapshot| usage_preferred_reset_at(snapshot, now)),
        used_percent: snapshot.and_then(usage_used_percent).unwrap_or_default(),
    }
}

fn score_is_better(score: CandidateScore, best_score: CandidateScore) -> bool {
    match (score.reset_at, best_score.reset_at) {
        (Some(reset_at), Some(best_reset_at)) if reset_at != best_reset_at => {
            reset_at < best_reset_at
        }
        (Some(_), None) => true,
        (None, Some(_)) => false,
        _ => score.used_percent < best_score.used_percent,
    }
}

fn blocked_until_for(
    state: &RateLimitSwitchState,
    snapshot_map: &HashMap<String, account_usage::StoredRateLimitSnapshot>,
    account_id: &str,
) -> Option<DateTime<Utc>> {
    state
        .blocked_until(account_id)
        .into_iter()
        .chain(
            snapshot_map
                .get(account_id)
                .and_then(usage_reset_blocked_until),
        )
        .max()
}

fn is_blocked(now: DateTime<Utc>, blocked_until: Option<DateTime<Utc>>) -> bool {
    blocked_until.is_some_and(|until| until > now)
}

fn has_unexpired_tried_marker(
    state: &RateLimitSwitchState,
    account_id: &str,
    now: DateTime<Utc>,
) -> bool {
    state.has_tried(account_id)
        && !state
            .blocked_until(account_id)
            .is_some_and(|blocked_until| blocked_until <= now)
}

pub fn select_next_account_id(
    codex_home: &Path,
    state: &RateLimitSwitchState,
    allow_api_key_fallback: bool,
    now: DateTime<Utc>,
    current_account_id: Option<&str>,
) -> io::Result<Option<String>> {
    let current = match current_account_id {
        Some(id) => Some(id.to_string()),
        None => codex_login::get_active_account_id(codex_home)?,
    };
    let accounts = codex_login::list_accounts(codex_home)?;

    let snapshots = account_usage::list_rate_limit_snapshots(codex_home).unwrap_or_default();
    let snapshot_map: HashMap<String, account_usage::StoredRateLimitSnapshot> = snapshots
        .into_iter()
        .map(|snapshot| (snapshot.account_id.clone(), snapshot))
        .collect();

    let mut chatgpt_accounts: Vec<&StoredAccount> = accounts
        .iter()
        .filter(|account| account.mode.has_chatgpt_account())
        .filter(|account| account_has_credentials(account))
        .collect();
    let mut api_key_accounts: Vec<&StoredAccount> = accounts
        .iter()
        .filter(|account| account.mode == AuthMode::ApiKey)
        .filter(|account| account_has_credentials(account))
        .collect();

    chatgpt_accounts.sort_by(|left, right| left.id.cmp(&right.id));
    api_key_accounts.sort_by(|left, right| left.id.cmp(&right.id));

    let current = current.as_deref();
    let mut best_chatgpt: Option<(&StoredAccount, CandidateScore)> = None;

    for account in &chatgpt_accounts {
        if current.is_some_and(|id| id == account.id) {
            continue;
        }
        if has_unexpired_tried_marker(state, &account.id, now) {
            continue;
        }
        if is_blocked(now, blocked_until_for(state, &snapshot_map, &account.id)) {
            continue;
        }

        let score = candidate_score(&snapshot_map, &account.id, now);
        match best_chatgpt {
            None => best_chatgpt = Some((*account, score)),
            Some((_, best_score)) if score_is_better(score, best_score) => {
                best_chatgpt = Some((*account, score));
            }
            Some(_) => {}
        }
    }

    if let Some((account, _)) = best_chatgpt {
        return Ok(Some(account.id.clone()));
    }

    if !allow_api_key_fallback {
        return Ok(None);
    }

    let all_chatgpt_unavailable = chatgpt_accounts.iter().all(|account| {
        current.is_some_and(|id| id == account.id)
            || has_unexpired_tried_marker(state, &account.id, now)
            || is_blocked(now, blocked_until_for(state, &snapshot_map, &account.id))
    });
    if !all_chatgpt_unavailable {
        return Ok(None);
    }

    Ok(api_key_accounts
        .into_iter()
        .find(|account| {
            !current.is_some_and(|id| id == account.id)
                && !has_unexpired_tried_marker(state, &account.id, now)
        })
        .map(|account| account.id.clone()))
}

pub fn switch_active_account_to_preferred_for_new_session(
    codex_home: &Path,
    auth_home: &Path,
    now: DateTime<Utc>,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<Option<StoredAccount>> {
    let current_account_id = codex_login::get_active_account_id(auth_home)?;
    let Some(current_account_id) = current_account_id else {
        return Ok(None);
    };
    let accounts = codex_login::list_accounts(auth_home)?;

    if let Some(current) = accounts
        .iter()
        .find(|account| account.id == current_account_id)
        && !current.mode.has_chatgpt_account()
    {
        return Ok(None);
    }

    let snapshots = account_usage::list_rate_limit_snapshots(codex_home).unwrap_or_default();
    let snapshot_map: HashMap<String, account_usage::StoredRateLimitSnapshot> = snapshots
        .into_iter()
        .map(|snapshot| (snapshot.account_id.clone(), snapshot))
        .collect();

    let mut best_chatgpt: Option<(&StoredAccount, CandidateScore)> = None;
    let mut chatgpt_accounts: Vec<&StoredAccount> = accounts
        .iter()
        .filter(|account| account.mode.has_chatgpt_account())
        .filter(|account| account_has_credentials(account))
        .collect();
    chatgpt_accounts.sort_by(|left, right| left.id.cmp(&right.id));

    for account in chatgpt_accounts {
        let blocked_until = snapshot_map
            .get(&account.id)
            .and_then(usage_reset_blocked_until);
        if is_blocked(now, blocked_until) {
            continue;
        }

        let score = candidate_score(&snapshot_map, &account.id, now);
        match best_chatgpt {
            None => best_chatgpt = Some((account, score)),
            Some((_, best_score)) if score_is_better(score, best_score) => {
                best_chatgpt = Some((account, score));
            }
            Some(_) => {}
        }
    }

    let Some((account, _)) = best_chatgpt else {
        return Ok(None);
    };

    if current_account_id == account.id {
        return Ok(None);
    }

    codex_login::activate_account(auth_home, &account.id, auth_credentials_store_mode).map(Some)
}

pub fn switch_active_account_on_rate_limit(
    codex_home: &Path,
    state: &mut RateLimitSwitchState,
    allow_api_key_fallback: bool,
    now: DateTime<Utc>,
    current_account_id: &str,
    blocked_until: Option<DateTime<Utc>>,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> io::Result<AccountSwitchOutcome> {
    state.mark_limited(current_account_id, blocked_until);
    match select_next_account_id(
        codex_home,
        state,
        allow_api_key_fallback,
        now,
        Some(current_account_id),
    )? {
        Some(account_id) => {
            codex_login::activate_account(codex_home, &account_id, auth_credentials_store_mode)
                .map(AccountSwitchOutcome::Switched)
        }
        None => Ok(AccountSwitchOutcome::NoCandidate),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use chrono::Duration;
    use codex_config::types::AuthCredentialsStoreMode;
    use codex_login::auth::AuthDotJson;
    use codex_login::auth::save_auth;
    use codex_login::token_data::IdTokenInfo;
    use codex_login::token_data::TokenData;
    use codex_protocol::protocol::RateLimitSnapshot;
    use codex_protocol::protocol::RateLimitWindow;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use serde_json::json;

    fn fake_jwt(account_id: &str, email: &str) -> String {
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }

        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let payload = json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_user_id": format!("user-{account_id}"),
                "chatgpt_account_id": account_id,
            },
        });
        let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let header_b64 = encode(&serde_json::to_vec(&header).expect("serialize header"));
        let payload_b64 = encode(&serde_json::to_vec(&payload).expect("serialize payload"));
        let signature_b64 = encode(b"sig");
        format!("{header_b64}.{payload_b64}.{signature_b64}")
    }

    fn token_data(account_id: &str, email: &str) -> TokenData {
        TokenData {
            id_token: IdTokenInfo {
                email: Some(email.to_string()),
                chatgpt_plan_type: None,
                chatgpt_user_id: Some(format!("user-{account_id}")),
                chatgpt_account_id: Some(account_id.to_string()),
                chatgpt_account_is_fedramp: false,
                raw_jwt: fake_jwt(account_id, email),
            },
            access_token: format!("access-{account_id}"),
            refresh_token: format!("refresh-{account_id}"),
            account_id: Some(account_id.to_string()),
        }
    }

    fn upsert_chatgpt(codex_home: &Path, account_id: &str) -> String {
        codex_login::upsert_chatgpt_account(
            codex_home,
            token_data(account_id, "user@example.com"),
            Utc::now(),
            None,
            false,
        )
        .expect("upsert chatgpt")
        .id
    }

    fn upsert_api_key(codex_home: &Path, key: &str) -> String {
        codex_login::upsert_api_key_account(codex_home, key.to_string(), None, false)
            .expect("upsert api key")
            .id
    }

    fn rate_limit_snapshot(resets_at_seconds: i64, used_percent: f64) -> RateLimitSnapshot {
        RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: Some("Codex".to_string()),
            primary: Some(RateLimitWindow {
                used_percent,
                window_minutes: Some(300),
                resets_at: Some(resets_at_seconds),
            }),
            secondary: None,
            credits: None,
            individual_limit: None,
            plan_type: None,
            rate_limit_reached_type: None,
        }
    }

    #[test]
    fn selects_best_chatgpt_candidate_and_skips_current() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let slower = upsert_chatgpt(temp.path(), "slower");
        let faster = upsert_chatgpt(temp.path(), "faster");
        codex_login::set_active_account_id(temp.path(), Some(current.clone())).expect("set active");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &slower,
            rate_limit_snapshot(now.timestamp() + 3 * 60 * 60, 10.0),
            now,
        )
        .expect("record slower");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &faster,
            rate_limit_snapshot(now.timestamp() + 60 * 60, 80.0),
            now,
        )
        .expect("record faster");

        let selected = select_next_account_id(
            temp.path(),
            &RateLimitSwitchState::default(),
            false,
            now,
            None,
        )
        .expect("select");

        assert_eq!(selected, Some(faster));
    }

    #[test]
    fn does_not_fallback_to_api_key_while_chatgpt_candidate_remains() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let candidate = upsert_chatgpt(temp.path(), "candidate");
        let _api_key = upsert_api_key(temp.path(), "sk-test");

        let selected = select_next_account_id(
            temp.path(),
            &RateLimitSwitchState::default(),
            true,
            now,
            Some(&current),
        )
        .expect("select");

        assert_eq!(selected, Some(candidate));
    }

    #[test]
    fn falls_back_to_api_key_after_chatgpt_accounts_are_unavailable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let blocked = upsert_chatgpt(temp.path(), "blocked");
        let api_key = upsert_api_key(temp.path(), "sk-test");
        account_usage::record_usage_limit_hint(
            temp.path(),
            &blocked,
            None,
            Some(now + Duration::hours(2)),
            now,
            Some(RateLimitReachedType::RateLimitReached),
        )
        .expect("record usage hint");

        let selected = select_next_account_id(
            temp.path(),
            &RateLimitSwitchState::default(),
            true,
            now,
            Some(&current),
        )
        .expect("select");

        assert_eq!(selected, Some(api_key));
    }

    #[test]
    fn api_key_fallback_skips_tried_accounts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let first_api_key = upsert_api_key(temp.path(), "sk-first");
        let second_api_key = upsert_api_key(temp.path(), "sk-second");
        let mut state = RateLimitSwitchState::default();
        state.mark_limited(&first_api_key, Some(now + Duration::hours(1)));

        let selected =
            select_next_account_id(temp.path(), &state, true, now, Some(&current)).expect("select");

        assert_eq!(selected, Some(second_api_key));
    }

    #[test]
    fn switch_active_account_marks_current_and_activates_candidate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let candidate = upsert_chatgpt(temp.path(), "candidate");
        codex_login::set_active_account_id(temp.path(), Some(current.clone())).expect("set active");
        let current_auth = AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: Some(token_data("current", "user@example.com")),
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
        };
        save_auth(temp.path(), &current_auth, AuthCredentialsStoreMode::File)
            .expect("save current auth");

        let mut state = RateLimitSwitchState::default();
        let outcome = switch_active_account_on_rate_limit(
            temp.path(),
            &mut state,
            false,
            now,
            &current,
            Some(now + Duration::hours(1)),
            AuthCredentialsStoreMode::File,
        )
        .expect("switch account");

        assert_eq!(
            outcome,
            AccountSwitchOutcome::Switched(
                codex_login::find_account(temp.path(), &candidate)
                    .expect("find account")
                    .expect("candidate account")
            )
        );
        assert_eq!(
            codex_login::get_active_account_id(temp.path()).expect("active account"),
            Some(candidate)
        );
        assert!(state.has_tried(&current));
    }

    #[test]
    fn new_session_switch_selects_best_chatgpt_candidate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let slower = upsert_chatgpt(temp.path(), "slower");
        let faster = upsert_chatgpt(temp.path(), "faster");
        codex_login::set_active_account_id(temp.path(), Some(current.clone())).expect("set active");
        save_auth(
            temp.path(),
            &AuthDotJson {
                auth_mode: None,
                openai_api_key: None,
                tokens: Some(token_data("current", "user@example.com")),
                last_refresh: None,
                agent_identity: None,
                personal_access_token: None,
            },
            AuthCredentialsStoreMode::File,
        )
        .expect("save current auth");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &slower,
            rate_limit_snapshot(now.timestamp() + 3 * 60 * 60, 10.0),
            now,
        )
        .expect("record slower");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &faster,
            rate_limit_snapshot(now.timestamp() + 60 * 60, 80.0),
            now,
        )
        .expect("record faster");

        let switched = switch_active_account_to_preferred_for_new_session(
            temp.path(),
            temp.path(),
            now,
            AuthCredentialsStoreMode::File,
        )
        .expect("switch preferred account")
        .expect("preferred account");

        assert_eq!(switched.id, faster);
        assert_eq!(
            codex_login::get_active_account_id(temp.path()).expect("active account"),
            Some(faster)
        );
    }

    #[test]
    fn new_session_switch_skips_when_current_account_is_api_key() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let api_key = upsert_api_key(temp.path(), "sk-current");
        let _chatgpt = upsert_chatgpt(temp.path(), "chatgpt");
        codex_login::set_active_account_id(temp.path(), Some(api_key.clone())).expect("set active");

        let switched = switch_active_account_to_preferred_for_new_session(
            temp.path(),
            temp.path(),
            now,
            AuthCredentialsStoreMode::File,
        )
        .expect("switch preferred account");

        assert_eq!(switched, None);
        assert_eq!(
            codex_login::get_active_account_id(temp.path()).expect("active account"),
            Some(api_key)
        );
    }

    #[test]
    fn new_session_switch_skips_when_no_active_account() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let _chatgpt = upsert_chatgpt(temp.path(), "chatgpt");
        codex_login::set_active_account_id(temp.path(), None).expect("clear active");

        let switched = switch_active_account_to_preferred_for_new_session(
            temp.path(),
            temp.path(),
            now,
            AuthCredentialsStoreMode::File,
        )
        .expect("switch preferred account");

        assert_eq!(switched, None);
        assert_eq!(
            codex_login::get_active_account_id(temp.path()).expect("active account"),
            None
        );
    }

    #[test]
    fn new_session_switch_skips_blocked_chatgpt_accounts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let blocked = upsert_chatgpt(temp.path(), "blocked");
        codex_login::set_active_account_id(temp.path(), Some(current.clone())).expect("set active");
        account_usage::record_usage_limit_hint(
            temp.path(),
            &blocked,
            None,
            Some(now + Duration::hours(2)),
            now,
            Some(RateLimitReachedType::RateLimitReached),
        )
        .expect("record usage hint");

        let switched = switch_active_account_to_preferred_for_new_session(
            temp.path(),
            temp.path(),
            now,
            AuthCredentialsStoreMode::File,
        )
        .expect("switch preferred account");

        assert_eq!(switched, None);
        assert_eq!(
            codex_login::get_active_account_id(temp.path()).expect("active account"),
            Some(current)
        );
    }
}
