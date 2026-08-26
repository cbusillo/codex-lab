use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::path::Path;

use chrono::DateTime;
use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::StoredAccount;
use codex_protocol::auth::AuthMode;

use crate::account_usage;
use codex_protocol::protocol::RateLimitReachedType;

#[derive(Debug, Default)]
pub struct RateLimitSwitchState {
    tried_accounts: HashSet<String>,
    limited_chatgpt_accounts: HashSet<String>,
    blocked_until: HashMap<String, DateTime<Utc>>,
}

impl RateLimitSwitchState {
    pub(crate) fn mark_tried(&mut self, account_id: &str) {
        self.tried_accounts.insert(account_id.to_string());
    }

    pub fn mark_limited(
        &mut self,
        account_id: &str,
        mode: AuthMode,
        blocked_until: Option<DateTime<Utc>>,
    ) {
        self.mark_tried(account_id);
        if mode.has_chatgpt_account() {
            self.limited_chatgpt_accounts.insert(account_id.to_string());
        }
        if let Some(until) = blocked_until {
            self.blocked_until
                .entry(account_id.to_string())
                .and_modify(|existing| {
                    if until > *existing {
                        *existing = until;
                    }
                })
                .or_insert(until);
        } else {
            self.blocked_until.remove(account_id);
        }
    }

    fn has_tried(&self, account_id: &str) -> bool {
        self.tried_accounts.contains(account_id)
    }

    fn blocked_until(&self, account_id: &str) -> Option<DateTime<Utc>> {
        self.blocked_until.get(account_id).copied()
    }

    fn is_chatgpt_limited(&self, account_id: &str) -> bool {
        self.limited_chatgpt_accounts.contains(account_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CandidateScore {
    reset_at: Option<DateTime<Utc>>,
    used_percent: f64,
}

fn account_has_credentials(account: &StoredAccount) -> bool {
    match account.mode {
        AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens => account.tokens.is_some(),
        AuthMode::ApiKey => account.openai_api_key.is_some(),
        AuthMode::PersonalAccessToken
        | AuthMode::AgentIdentity
        | AuthMode::Headers
        | AuthMode::BedrockApiKey
        | AuthMode::BedrockAccessKeys => false,
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
    primary_reset.into_iter().chain(secondary_reset).min()
}

fn legacy_usage_account_id(account: &StoredAccount) -> Option<&str> {
    account.tokens.as_ref().and_then(|tokens| {
        tokens
            .account_id
            .as_deref()
            .filter(|account_id| !account_id.is_empty())
            .or_else(|| {
                tokens
                    .id_token
                    .chatgpt_account_id
                    .as_deref()
                    .filter(|account_id| !account_id.is_empty())
            })
    })
}

fn usage_snapshot_for_account<'a>(
    snapshot_map: &'a HashMap<String, account_usage::StoredRateLimitSnapshot>,
    account: &StoredAccount,
) -> Option<&'a account_usage::StoredRateLimitSnapshot> {
    snapshot_map.get(&account.id).or_else(|| {
        legacy_usage_account_id(account).and_then(|account_id| snapshot_map.get(account_id))
    })
}

fn candidate_score(
    snapshot_map: &HashMap<String, account_usage::StoredRateLimitSnapshot>,
    account: &StoredAccount,
    now: DateTime<Utc>,
) -> CandidateScore {
    let snapshot = usage_snapshot_for_account(snapshot_map, account);
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
    account: &StoredAccount,
) -> Option<DateTime<Utc>> {
    state
        .blocked_until(&account.id)
        .into_iter()
        .chain(
            usage_snapshot_for_account(snapshot_map, account).and_then(usage_reset_blocked_until),
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
        && state
            .blocked_until(account_id)
            .is_none_or(|blocked_until| blocked_until > now)
}

pub fn select_next_account_id(
    codex_home: &Path,
    auth_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    state: &RateLimitSwitchState,
    allow_api_key_fallback: bool,
    now: DateTime<Utc>,
    current_account_id: Option<&str>,
) -> io::Result<Option<String>> {
    let current = match current_account_id {
        Some(id) => Some(id.to_string()),
        None => codex_login::get_active_account_id(auth_home, auth_credentials_store_mode)?,
    };
    select_account_id(
        codex_home,
        auth_home,
        auth_credentials_store_mode,
        state,
        allow_api_key_fallback,
        now,
        current.as_deref(),
    )
}

fn select_account_id(
    codex_home: &Path,
    auth_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    state: &RateLimitSwitchState,
    allow_api_key_fallback: bool,
    now: DateTime<Utc>,
    current_account_id: Option<&str>,
) -> io::Result<Option<String>> {
    let accounts = codex_login::list_accounts(auth_home, auth_credentials_store_mode)?;

    let snapshots = account_usage::list_rate_limit_snapshots(codex_home).unwrap_or_default();
    let snapshot_map: HashMap<String, account_usage::StoredRateLimitSnapshot> = snapshots
        .into_iter()
        .map(|snapshot| (snapshot.account_id.clone(), snapshot))
        .collect();

    let mut chatgpt_accounts: Vec<&StoredAccount> = accounts
        .iter()
        .filter(|account| account.health.is_ok())
        .filter(|account| account.mode.has_chatgpt_account())
        .filter(|account| account_has_credentials(account))
        .collect();
    let mut api_key_accounts: Vec<&StoredAccount> = accounts
        .iter()
        .filter(|account| account.health.is_ok())
        .filter(|account| account.mode == AuthMode::ApiKey)
        .filter(|account| account_has_credentials(account))
        .collect();

    chatgpt_accounts.sort_by(|left, right| left.id.cmp(&right.id));
    api_key_accounts.sort_by(|left, right| left.id.cmp(&right.id));

    let current = current_account_id;
    let mut best_chatgpt: Option<(&StoredAccount, CandidateScore)> = None;

    for account in &chatgpt_accounts {
        if current.is_some_and(|id| id == account.id) {
            continue;
        }
        if state.is_chatgpt_limited(&account.id) {
            continue;
        }
        if has_unexpired_tried_marker(state, &account.id, now) {
            continue;
        }
        if is_blocked(now, blocked_until_for(state, &snapshot_map, account)) {
            continue;
        }

        let score = candidate_score(&snapshot_map, account, now);
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
        let blocked_until = blocked_until_for(state, &snapshot_map, account);
        let blocked = is_blocked(now, blocked_until);
        let exhausted = state.is_chatgpt_limited(&account.id);
        let tried = state.has_tried(&account.id);
        current.is_some_and(|id| id == account.id) || blocked || (tried && exhausted)
    });
    if !all_chatgpt_unavailable {
        return Ok(None);
    }

    Ok(api_key_accounts
        .into_iter()
        .find(|account| {
            current.is_none_or(|id| id != account.id)
                && !has_unexpired_tried_marker(state, &account.id, now)
        })
        .map(|account| account.id.clone()))
}

pub fn select_preferred_account_id(
    codex_home: &Path,
    auth_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    allow_api_key_fallback: bool,
    now: DateTime<Utc>,
) -> io::Result<Option<String>> {
    let state = RateLimitSwitchState::default();
    select_account_id(
        codex_home,
        auth_home,
        auth_credentials_store_mode,
        &state,
        allow_api_key_fallback,
        now,
        /*current_account_id*/ None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use chrono::Duration;
    use codex_config::types::AuthCredentialsStoreMode;
    use codex_login::token_data::IdTokenInfo;
    use codex_login::token_data::TokenData;
    use codex_protocol::protocol::RateLimitSnapshot;
    use codex_protocol::protocol::RateLimitWindow;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use serde_json::json;

    fn select_next_account_id(
        codex_home: &Path,
        auth_home: &Path,
        state: &RateLimitSwitchState,
        allow_api_key_fallback: bool,
        now: DateTime<Utc>,
        current_account_id: Option<&str>,
    ) -> io::Result<Option<String>> {
        super::select_next_account_id(
            codex_home,
            auth_home,
            AuthCredentialsStoreMode::File,
            state,
            allow_api_key_fallback,
            now,
            current_account_id,
        )
    }

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
            AuthCredentialsStoreMode::File,
            token_data(account_id, "user@example.com"),
            Utc::now(),
            /*label*/ None,
            /*make_active*/ false,
        )
        .expect("upsert chatgpt")
        .id
    }

    fn set_account_health(codex_home: &Path, account_id: &str, health: &str) {
        let path = codex_home.join("auth_accounts.json");
        let mut catalog: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read account catalog"))
                .expect("parse account catalog");
        let account = catalog["accounts"]
            .as_array_mut()
            .expect("accounts array")
            .iter_mut()
            .find(|account| account["id"] == account_id)
            .expect("catalog account");
        account["health"] = serde_json::Value::String(health.to_string());
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&catalog).expect("serialize account catalog"),
        )
        .expect("write account catalog");
    }

    fn upsert_claim_only_chatgpt(codex_home: &Path, account_id: &str) -> String {
        let mut tokens = token_data(account_id, "user@example.com");
        tokens.account_id = None;
        codex_login::upsert_chatgpt_account(
            codex_home,
            AuthCredentialsStoreMode::File,
            tokens,
            Utc::now(),
            /*label*/ None,
            /*make_active*/ false,
        )
        .expect("upsert claim-only chatgpt")
        .id
    }

    fn upsert_api_key(codex_home: &Path, key: &str) -> String {
        codex_login::upsert_api_key_account(
            codex_home,
            AuthCredentialsStoreMode::File,
            key.to_string(),
            /*label*/ None,
            /*make_active*/ false,
        )
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
            spend_control_reached: None,
            plan_type: None,
            rate_limit_reached_type: None,
        }
    }

    #[test]
    fn preferred_reset_uses_earliest_active_window() {
        let now = Utc::now();
        let snapshot = account_usage::StoredRateLimitSnapshot {
            account_id: "account".to_string(),
            plan: None,
            snapshot: None,
            observed_at: Some(now),
            primary_next_reset_at: Some(now + Duration::hours(5)),
            secondary_next_reset_at: Some(now + Duration::days(5)),
            last_usage_limit_hit_at: None,
        };

        assert_eq!(
            usage_preferred_reset_at(&snapshot, now),
            snapshot.primary_next_reset_at
        );
    }

    #[test]
    fn selects_best_chatgpt_candidate_and_skips_current() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let slower = upsert_chatgpt(temp.path(), "slower");
        let faster = upsert_claim_only_chatgpt(temp.path(), "faster");
        codex_login::set_active_account_id(
            temp.path(),
            AuthCredentialsStoreMode::File,
            Some(current),
        )
        .expect("set active");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &slower,
            rate_limit_snapshot(now.timestamp() + 3 * 60 * 60, /*used_percent*/ 10.0),
            now,
        )
        .expect("record slower");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            "faster",
            rate_limit_snapshot(now.timestamp() + 60 * 60, /*used_percent*/ 80.0),
            now,
        )
        .expect("record faster");

        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            now,
            /*current_account_id*/ None,
        )
        .expect("select");

        assert_eq!(selected, Some(faster));
    }

    #[test]
    fn preferred_selection_excludes_reauth_required_account() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let first = upsert_chatgpt(temp.path(), "first");
        let second = upsert_chatgpt(temp.path(), "second");
        let (unhealthy, healthy) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        set_account_health(temp.path(), &unhealthy, "reauth_required");
        codex_login::set_active_account_id(
            temp.path(),
            AuthCredentialsStoreMode::File,
            Some(healthy.clone()),
        )
        .expect("set healthy active account");

        let selected = super::select_preferred_account_id(
            temp.path(),
            temp.path(),
            AuthCredentialsStoreMode::File,
            /*allow_api_key_fallback*/ false,
            now,
        )
        .expect("select preferred account");

        assert_eq!(selected, Some(healthy.clone()));
        assert_eq!(
            codex_login::get_active_account_id(temp.path(), AuthCredentialsStoreMode::File,)
                .expect("read active account"),
            Some(healthy)
        );
    }

    #[test]
    fn stored_usage_key_precedes_legacy_chatgpt_key() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let preferred = upsert_chatgpt(temp.path(), "preferred");
        let alternate = upsert_chatgpt(temp.path(), "alternate");
        account_usage::record_usage_limit_hint(
            temp.path(),
            "preferred",
            /*plan*/ None,
            Some(now + Duration::hours(2)),
            now,
            Some(RateLimitReachedType::RateLimitReached),
        )
        .expect("record legacy snapshot");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &preferred,
            rate_limit_snapshot(now.timestamp() + 60 * 60, /*used_percent*/ 10.0),
            now,
        )
        .expect("record stored snapshot");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &alternate,
            rate_limit_snapshot(now.timestamp() + 3 * 60 * 60, /*used_percent*/ 10.0),
            now,
        )
        .expect("record alternate snapshot");

        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            now,
            Some(&current),
        )
        .expect("select");
        assert_eq!(
            selected,
            Some(preferred),
            "stored-key snapshot should take precedence over blocked legacy history"
        );
    }

    #[test]
    fn current_account_override_takes_precedence_over_stored_active_account() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let stored_active = upsert_chatgpt(temp.path(), "stored-active");
        let current_session = upsert_chatgpt(temp.path(), "current-session");
        codex_login::set_active_account_id(
            temp.path(),
            AuthCredentialsStoreMode::File,
            Some(stored_active.clone()),
        )
        .expect("set stored active account");
        let mut state = RateLimitSwitchState::default();
        state.mark_limited(
            &current_session,
            AuthMode::Chatgpt,
            /*blocked_until*/ None,
        );

        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &state,
            /*allow_api_key_fallback*/ false,
            now,
            Some(&current_session),
        )
        .expect("select next account");

        assert_eq!(selected, Some(stored_active));
    }

    #[test]
    fn current_account_override_is_not_reselected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let stored_active = upsert_chatgpt(temp.path(), "stored-active");
        let current_session = upsert_chatgpt(temp.path(), "current-session");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &stored_active,
            rate_limit_snapshot(now.timestamp() + 3 * 60 * 60, /*used_percent*/ 80.0),
            now,
        )
        .expect("record stored active snapshot");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &current_session,
            rate_limit_snapshot(now.timestamp() + 60 * 60, /*used_percent*/ 10.0),
            now,
        )
        .expect("record current session snapshot");

        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            now,
            Some(&current_session),
        )
        .expect("select next account");

        assert_eq!(selected, Some(stored_active));
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
            temp.path(),
            &RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ true,
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
        let _blocked = upsert_chatgpt(temp.path(), "blocked");
        let api_key = upsert_api_key(temp.path(), "sk-test");
        account_usage::record_usage_limit_hint(
            temp.path(),
            "blocked",
            /*plan*/ None,
            Some(now + Duration::hours(2)),
            now,
            Some(RateLimitReachedType::RateLimitReached),
        )
        .expect("record usage hint");

        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ true,
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
        state.mark_limited(
            &first_api_key,
            AuthMode::ApiKey,
            Some(now + Duration::hours(1)),
        );

        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &state,
            /*allow_api_key_fallback*/ true,
            now,
            Some(&current),
        )
        .expect("select");

        assert_eq!(selected, Some(second_api_key));
    }

    #[test]
    fn api_key_fallback_requires_all_chatgpt_accounts_marked_limited() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let candidate = upsert_chatgpt(temp.path(), "candidate");
        let api_key = upsert_api_key(temp.path(), "sk-test");
        let mut state = RateLimitSwitchState::default();
        state.mark_limited(&current, AuthMode::Chatgpt, /*blocked_until*/ None);

        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &state,
            /*allow_api_key_fallback*/ true,
            now,
            Some(&current),
        )
        .expect("select");
        assert_eq!(selected, Some(candidate.clone()));

        state.mark_limited(&candidate, AuthMode::Chatgpt, /*blocked_until*/ None);
        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &state,
            /*allow_api_key_fallback*/ true,
            now,
            Some(&candidate),
        )
        .expect("select");
        assert_eq!(selected, Some(api_key));
    }

    #[test]
    fn limited_chatgpt_account_is_not_reselected_without_reset_hint() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let candidate = upsert_chatgpt(temp.path(), "candidate");
        let api_key = upsert_api_key(temp.path(), "sk-test");
        let mut state = RateLimitSwitchState::default();
        state.mark_limited(&current, AuthMode::Chatgpt, /*blocked_until*/ None);

        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &state,
            /*allow_api_key_fallback*/ true,
            now,
            Some(&current),
        )
        .expect("select");
        assert_eq!(selected, Some(candidate.clone()));

        state.mark_limited(&candidate, AuthMode::Chatgpt, /*blocked_until*/ None);
        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &state,
            /*allow_api_key_fallback*/ true,
            now,
            Some(&candidate),
        )
        .expect("select");
        assert_eq!(selected, Some(api_key));
    }

    #[test]
    fn limited_chatgpt_account_is_not_reselected_after_expired_reset_hint() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let candidate = upsert_chatgpt(temp.path(), "candidate");
        let api_key = upsert_api_key(temp.path(), "sk-test");
        let mut state = RateLimitSwitchState::default();
        state.mark_limited(
            &current,
            AuthMode::Chatgpt,
            Some(now - Duration::minutes(1)),
        );

        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &state,
            /*allow_api_key_fallback*/ true,
            now,
            Some(&current),
        )
        .expect("select");
        assert_eq!(selected, Some(candidate.clone()));

        state.mark_limited(
            &candidate,
            AuthMode::Chatgpt,
            Some(now - Duration::minutes(1)),
        );
        let selected = select_next_account_id(
            temp.path(),
            temp.path(),
            &state,
            /*allow_api_key_fallback*/ true,
            now,
            Some(&candidate),
        )
        .expect("select");
        assert_eq!(selected, Some(api_key));
    }

    #[test]
    fn preferred_selection_does_not_change_active_account() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let current = upsert_chatgpt(temp.path(), "current");
        let slower = upsert_chatgpt(temp.path(), "slower");
        let faster = upsert_chatgpt(temp.path(), "faster");
        codex_login::set_active_account_id(
            temp.path(),
            AuthCredentialsStoreMode::File,
            Some(current.clone()),
        )
        .expect("set active");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            &slower,
            rate_limit_snapshot(now.timestamp() + 3 * 60 * 60, /*used_percent*/ 10.0),
            now,
        )
        .expect("record slower");
        account_usage::record_rate_limit_snapshot(
            temp.path(),
            "faster",
            rate_limit_snapshot(now.timestamp() + 60 * 60, /*used_percent*/ 80.0),
            now,
        )
        .expect("record faster");

        let selected = select_preferred_account_id(
            temp.path(),
            temp.path(),
            AuthCredentialsStoreMode::File,
            /*allow_api_key_fallback*/ false,
            now,
        )
        .expect("select preferred account");

        assert_eq!(selected, Some(faster));
        assert_eq!(
            codex_login::get_active_account_id(temp.path(), AuthCredentialsStoreMode::File,)
                .expect("active account"),
            Some(current)
        );
    }
}
