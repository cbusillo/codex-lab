use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_protocol::account::PlanType;
use codex_protocol::protocol::RateLimitReachedType;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::TokenUsage;
use fs2::FileExt;
use serde::Deserialize;
use serde::Serialize;

const USAGE_VERSION: u32 = 1;
const USAGE_SUBDIR: &str = "usage";
const RATE_LIMIT_REFRESH_STALE_INTERVAL_SECS: i64 = 30 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TokenTotals {
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub cached_input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub reasoning_output_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

impl TokenTotals {
    fn add_usage(&mut self, usage: &TokenUsage) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(usage.cached_input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.reasoning_output_tokens = self
            .reasoning_output_tokens
            .saturating_add(usage.reasoning_output_tokens);
        self.total_tokens = self.total_tokens.saturating_add(usage.total_tokens);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RateLimitInfo {
    #[serde(default)]
    snapshot: Option<RateLimitSnapshot>,
    #[serde(default)]
    observed_at: Option<DateTime<Utc>>,
    #[serde(default, alias = "next_reset_at")]
    primary_next_reset_at: Option<DateTime<Utc>>,
    #[serde(default)]
    secondary_next_reset_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_refresh_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_usage_limit_hit_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_usage_limit_reached_type: Option<RateLimitReachedType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccountUsageData {
    version: u32,
    account_id: String,
    #[serde(default)]
    plan: Option<PlanType>,
    #[serde(default)]
    last_updated: DateTime<Utc>,
    #[serde(default)]
    totals: TokenTotals,
    #[serde(default)]
    rate_limit: Option<RateLimitInfo>,
}

impl AccountUsageData {
    fn new(account_id: String) -> Self {
        Self {
            version: USAGE_VERSION,
            account_id,
            plan: None,
            last_updated: Utc::now(),
            totals: TokenTotals::default(),
            rate_limit: None,
        }
    }

    fn apply_plan(&mut self, plan: Option<PlanType>) {
        if let Some(plan) = plan
            && self.plan.as_ref() != Some(&plan)
        {
            self.plan = Some(plan);
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredRateLimitSnapshot {
    pub account_id: String,
    pub plan: Option<PlanType>,
    pub snapshot: Option<RateLimitSnapshot>,
    pub observed_at: Option<DateTime<Utc>>,
    pub primary_next_reset_at: Option<DateTime<Utc>>,
    pub secondary_next_reset_at: Option<DateTime<Utc>>,
    pub last_usage_limit_hit_at: Option<DateTime<Utc>>,
}

fn usage_dir(codex_home: &Path) -> PathBuf {
    codex_home.join(USAGE_SUBDIR)
}

fn usage_file(codex_home: &Path, account_id: &str) -> PathBuf {
    usage_dir(codex_home).join(format!("{}.json", sanitize_account_id(account_id)))
}

fn sanitize_account_id(account_id: &str) -> String {
    account_id
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect()
}

fn read_locked_usage_file(file: &mut fs::File, account_id: &str) -> io::Result<AccountUsageData> {
    file.seek(SeekFrom::Start(0))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    if contents.trim().is_empty() {
        return Ok(AccountUsageData::new(account_id.to_string()));
    }
    serde_json::from_str(&contents).map_err(io::Error::other)
}

fn write_locked_usage_file(file: &mut fs::File, data: &AccountUsageData) -> io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer_pretty(&mut *file, data).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_data()
}

fn with_usage_file<F>(
    codex_home: &Path,
    account_id: &str,
    plan: Option<PlanType>,
    update: F,
) -> io::Result<()>
where
    F: FnOnce(&mut AccountUsageData),
{
    let path = usage_file(codex_home, account_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&path)?;
    file.lock_exclusive()?;
    let mut data = read_locked_usage_file(&mut file, account_id)?;
    data.version = USAGE_VERSION;
    data.account_id = account_id.to_string();
    data.apply_plan(plan);
    update(&mut data);
    let write_result = write_locked_usage_file(&mut file, &data);
    let unlock_result = file.unlock();
    write_result.and(unlock_result)
}

pub fn record_token_usage(
    codex_home: &Path,
    account_id: &str,
    plan: Option<PlanType>,
    usage: &TokenUsage,
    observed_at: DateTime<Utc>,
) -> io::Result<()> {
    with_usage_file(codex_home, account_id, plan, |data| {
        data.last_updated = observed_at;
        data.totals.add_usage(usage);
    })
}

pub fn record_rate_limit_snapshot(
    codex_home: &Path,
    account_id: &str,
    snapshot: RateLimitSnapshot,
    observed_at: DateTime<Utc>,
) -> io::Result<()> {
    record_rate_limit_snapshot_with_plan(
        codex_home,
        account_id,
        snapshot.plan_type.clone(),
        snapshot,
        observed_at,
    )
}

pub fn record_rate_limit_snapshot_with_plan(
    codex_home: &Path,
    account_id: &str,
    plan: Option<PlanType>,
    snapshot: RateLimitSnapshot,
    observed_at: DateTime<Utc>,
) -> io::Result<()> {
    let primary_next_reset_at = snapshot
        .primary
        .as_ref()
        .and_then(|window| unix_seconds_to_datetime(window.resets_at));
    let secondary_next_reset_at = snapshot
        .secondary
        .as_ref()
        .and_then(|window| unix_seconds_to_datetime(window.resets_at));
    with_usage_file(codex_home, account_id, plan, |data| {
        data.last_updated = observed_at;
        let mut info = data.rate_limit.take().unwrap_or_default();
        let existing_primary_next_reset_at = info.primary_next_reset_at;
        let existing_secondary_next_reset_at = info.secondary_next_reset_at;
        let existing_last_usage_limit_hit_at = info.last_usage_limit_hit_at;
        let existing_reached_type = info
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.rate_limit_reached_type)
            .or(info.last_usage_limit_reached_type);
        let reached_type = snapshot.rate_limit_reached_type.or(existing_reached_type);
        info.snapshot = Some(snapshot);
        if let Some(snapshot) = info.snapshot.as_mut() {
            snapshot.rate_limit_reached_type = reached_type;
        }
        info.observed_at = Some(observed_at);
        info.primary_next_reset_at = primary_next_reset_at.or(existing_primary_next_reset_at);
        info.secondary_next_reset_at = secondary_next_reset_at.or(existing_secondary_next_reset_at);
        info.last_usage_limit_hit_at = existing_last_usage_limit_hit_at;
        data.rate_limit = Some(info);
    })
}

fn unix_seconds_to_datetime(resets_at: Option<i64>) -> Option<DateTime<Utc>> {
    resets_at.and_then(|seconds| DateTime::from_timestamp(seconds, 0))
}

pub fn list_rate_limit_snapshots(codex_home: &Path) -> io::Result<Vec<StoredRateLimitSnapshot>> {
    let mut results = Vec::new();
    let dir = usage_dir(codex_home);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(results),
        Err(err) => return Err(err),
    };

    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
        {
            let contents = match fs::read_to_string(entry.path()) {
                Ok(contents) => contents,
                Err(_) => continue,
            };
            let data: AccountUsageData = match serde_json::from_str(&contents) {
                Ok(data) => data,
                Err(_) => continue,
            };
            let rate_limit = data.rate_limit.unwrap_or_default();
            results.push(StoredRateLimitSnapshot {
                account_id: data.account_id,
                plan: data.plan,
                snapshot: rate_limit.snapshot,
                observed_at: rate_limit.observed_at,
                primary_next_reset_at: rate_limit.primary_next_reset_at,
                secondary_next_reset_at: rate_limit
                    .secondary_next_reset_at
                    .or(rate_limit.primary_next_reset_at),
                last_usage_limit_hit_at: rate_limit.last_usage_limit_hit_at,
            });
        }
    }

    results.sort_by(|left, right| left.account_id.cmp(&right.account_id));
    Ok(results)
}

pub fn record_usage_limit_hint(
    codex_home: &Path,
    account_id: &str,
    plan: Option<PlanType>,
    resets_at: Option<DateTime<Utc>>,
    observed_at: DateTime<Utc>,
    reached_type: Option<RateLimitReachedType>,
) -> io::Result<()> {
    with_usage_file(codex_home, account_id, plan, |data| {
        data.last_updated = observed_at;
        let mut info = data.rate_limit.take().unwrap_or_default();
        info.last_usage_limit_hit_at = Some(observed_at);
        info.last_usage_limit_reached_type = reached_type;
        if let Some(snapshot) = info.snapshot.as_mut() {
            snapshot.rate_limit_reached_type = reached_type;
        }
        if let Some(resets_at) = resets_at {
            info.primary_next_reset_at = Some(resets_at);
            info.secondary_next_reset_at = Some(resets_at);
        }
        data.rate_limit = Some(info);
    })
}

pub fn rate_limit_refresh_stale_interval() -> Duration {
    Duration::seconds(RATE_LIMIT_REFRESH_STALE_INTERVAL_SECS)
}

pub fn mark_rate_limit_refresh_attempt_if_due(
    codex_home: &Path,
    account_id: &str,
    plan: Option<PlanType>,
    reset_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    stale_interval: Duration,
) -> io::Result<bool> {
    let mut should_refresh = false;
    with_usage_file(codex_home, account_id, plan, |data| {
        let mut info = data.rate_limit.take().unwrap_or_default();
        let refresh_due = match reset_at {
            Some(reset_at) if now < reset_at => false,
            _ => info
                .last_refresh_attempt_at
                .is_none_or(|attempt| now.signed_duration_since(attempt) >= stale_interval),
        };
        if refresh_due {
            info.last_refresh_attempt_at = Some(now);
            should_refresh = true;
        }
        data.rate_limit = Some(info);
    })?;
    Ok(should_refresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::account::PlanType;
    use codex_protocol::protocol::RateLimitWindow;

    fn snapshot(resets_in_seconds: i64, used_percent: f64) -> RateLimitSnapshot {
        RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: Some("Codex".to_string()),
            primary: Some(RateLimitWindow {
                used_percent,
                window_minutes: Some(300),
                resets_at: Some(resets_in_seconds),
            }),
            secondary: None,
            credits: None,
            individual_limit: None,
            spend_control_reached: None,
            plan_type: Some(PlanType::Plus),
            rate_limit_reached_type: None,
        }
    }

    #[test]
    fn records_and_lists_rate_limit_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let reset_at_seconds = 3600;
        let reset_at = DateTime::from_timestamp(reset_at_seconds, 0);
        record_rate_limit_snapshot(
            temp.path(),
            "acct/one",
            snapshot(reset_at_seconds, 42.0),
            now,
        )
        .expect("record snapshot");

        let snapshots = list_rate_limit_snapshots(temp.path()).expect("list snapshots");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].account_id, "acct/one");
        assert_eq!(snapshots[0].plan, Some(PlanType::Plus));
        assert_eq!(snapshots[0].primary_next_reset_at, reset_at);
        assert!(temp.path().join("usage").join("acct_one.json").exists());
    }

    #[test]
    fn usage_limit_hint_updates_existing_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let reset_at = now + Duration::hours(2);
        record_rate_limit_snapshot(temp.path(), "acct", snapshot(3600, 1.0), now)
            .expect("record snapshot");
        record_usage_limit_hint(
            temp.path(),
            "acct",
            Some(PlanType::Pro),
            Some(reset_at),
            now,
            Some(RateLimitReachedType::RateLimitReached),
        )
        .expect("record hint");

        let snapshots = list_rate_limit_snapshots(temp.path()).expect("list snapshots");
        assert_eq!(snapshots[0].plan, Some(PlanType::Pro));
        assert_eq!(snapshots[0].primary_next_reset_at, Some(reset_at));
        assert_eq!(snapshots[0].secondary_next_reset_at, Some(reset_at));
        assert_eq!(snapshots[0].last_usage_limit_hit_at, Some(now));
        assert_eq!(
            snapshots[0]
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.rate_limit_reached_type),
            Some(RateLimitReachedType::RateLimitReached)
        );
    }

    #[test]
    fn snapshot_recording_preserves_usage_limit_hint_fields() {
        let temp = tempfile::tempdir().expect("tempdir");
        let now = Utc::now();
        let reset_at = now + Duration::hours(2);
        record_usage_limit_hint(
            temp.path(),
            "acct",
            Some(PlanType::Plus),
            Some(reset_at),
            now,
            Some(RateLimitReachedType::RateLimitReached),
        )
        .expect("record hint");

        record_rate_limit_snapshot(
            temp.path(),
            "acct",
            RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: Some("Codex".to_string()),
                primary: Some(RateLimitWindow {
                    used_percent: 88.0,
                    window_minutes: Some(300),
                    resets_at: None,
                }),
                secondary: None,
                credits: None,
                individual_limit: None,
                spend_control_reached: None,
                plan_type: Some(PlanType::Plus),
                rate_limit_reached_type: None,
            },
            now + Duration::minutes(1),
        )
        .expect("record snapshot");

        let snapshots = list_rate_limit_snapshots(temp.path()).expect("list snapshots");
        assert_eq!(snapshots[0].primary_next_reset_at, Some(reset_at));
        assert_eq!(snapshots[0].secondary_next_reset_at, Some(reset_at));
        assert_eq!(snapshots[0].last_usage_limit_hit_at, Some(now));
        assert_eq!(
            snapshots[0]
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.rate_limit_reached_type),
            Some(RateLimitReachedType::RateLimitReached)
        );
    }
}
