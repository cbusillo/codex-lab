use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use anyhow::Result;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewTarget;
use codex_utils_path::write_atomically;
use serde::Deserialize;
use serde::Serialize;

mod review_coord;

pub use review_coord::ReviewCoordination;
pub use review_coord::ReviewLockGuard;
pub use review_coord::ReviewLockInfo;

pub const SUMMARY_MAX_FINDINGS: usize = 20;
pub const SUMMARY_MAX_FIELD_BYTES: usize = 240;
pub const SUMMARY_MAX_BYTES: usize = 4096;
pub const DETAIL_MAX_BYTES: usize = 16384;
pub const DETAIL_MAX_FINDINGS: usize = 10;
pub const SCHEMA_VERSION: u32 = 1;

const DEFAULT_MAX_RUNS: usize = 500;
const LEDGER_HIGH_TOKEN_COUNT: u64 = 25_000;
const LEDGER_HIGH_PROMPT_TOKEN_ESTIMATE: u64 = 32_000;
const LEDGER_LONG_ELAPSED_SECS: i64 = 5 * 60;
const STORE_DIR: &str = "auto-review";
const STATE_DIR: &str = "state";
const REVIEW_DIR: &str = "review";
const RUNS_FILENAME: &str = "runs.json";
const RUN_METADATA_DIR: &str = "run-metadata";
const RUN_STATES_DIR: &str = "run-states";
const OUTPUTS_DIR: &str = "outputs";
const OMITTED_TEMPLATE_PREFIX: &str = "... ";
const OMITTED_TEMPLATE_SUFFIX: &str = " more finding(s) omitted";
const DUPLICATE_AUTO_REVIEW_SCOPE_CANCEL_REASON: &str = "duplicate_auto_review_scope";

static AUTO_REVIEW_RUN_STATE_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone)]
pub struct AutoReviewStore {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReviewDuplicateMatch {
    pub run_id: String,
    pub status: AutoReviewRunStatus,
    pub disposition: AutoReviewDuplicateDisposition,
    pub finding_count: usize,
    pub model: Option<String>,
    pub token_count: Option<u64>,
    pub prompt_token_estimate: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReviewRunProjection {
    pub run_id: String,
    pub status: AutoReviewRunStatus,
    pub source: AutoReviewRunSource,
    pub freshness: AutoReviewFreshness,
    pub target_matches: bool,
    pub started_at_unix_secs: i64,
    pub completed_at_unix_secs: Option<i64>,
    pub model: Option<String>,
    pub error_summary: Option<String>,
    pub summary: AutoReviewSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReviewStatusCount {
    pub status: AutoReviewRunStatus,
    pub source: AutoReviewRunSource,
    pub freshness: AutoReviewFreshness,
    pub target_matches: bool,
    pub count: usize,
}

impl AutoReviewStatusCount {
    pub fn label(&self) -> String {
        let target = if self.target_matches {
            "target_current"
        } else {
            "off_target"
        };
        format!(
            "{}/{}/{}/{}",
            source_label(&self.source),
            status_label(&self.status),
            freshness_label(self.freshness),
            target,
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoReviewLedgerProjection {
    pub latest: Option<AutoReviewRunProjection>,
    pub current: Option<AutoReviewRunProjection>,
    pub status_counts: Vec<AutoReviewStatusCount>,
    pub diagnostics: Option<AutoReviewDiagnostics>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoReviewDiagnostics {
    pub recent_runs: usize,
    pub in_flight_runs: usize,
    pub terminal_runs: usize,
    pub skipped_runs: usize,
    pub duplicate_skipped_runs: usize,
    pub superseded_runs: usize,
    pub failed_runs: usize,
    pub cancelled_runs: usize,
    pub lost_runs: usize,
    pub suppressed_stale_runs: usize,
    pub token_count: u64,
    pub token_runs: usize,
    pub prompt_token_estimate: u64,
    pub prompt_runs: usize,
    pub saved_token_estimate: u64,
    pub saved_runs: usize,
    pub high_burn_runs: usize,
    pub longest_elapsed_bucket: Option<&'static str>,
}

impl AutoReviewDiagnostics {
    pub fn from_runs<'a>(
        runs: impl IntoIterator<Item = &'a AutoReviewRun>,
        active_target: Option<&AutoReviewRunTarget>,
        active_review_target: Option<&ReviewTarget>,
    ) -> Option<Self> {
        let mut diagnostics = Self::default();
        for run in runs {
            diagnostics.recent_runs += 1;
            if run.status.is_in_flight() {
                diagnostics.in_flight_runs += 1;
            } else {
                diagnostics.terminal_runs += 1;
            }
            match &run.status {
                AutoReviewRunStatus::Superseded => diagnostics.superseded_runs += 1,
                AutoReviewRunStatus::Skipped => {
                    diagnostics.skipped_runs += 1;
                    if run.is_duplicate_skipped() {
                        diagnostics.duplicate_skipped_runs += 1;
                    }
                }
                AutoReviewRunStatus::Failed => diagnostics.failed_runs += 1,
                AutoReviewRunStatus::Cancelled => diagnostics.cancelled_runs += 1,
                AutoReviewRunStatus::Lost => diagnostics.lost_runs += 1,
                AutoReviewRunStatus::Pending
                | AutoReviewRunStatus::Snapshotting
                | AutoReviewRunStatus::Running
                | AutoReviewRunStatus::Reviewing
                | AutoReviewRunStatus::Resolving
                | AutoReviewRunStatus::Completed => {}
            }
            if let (Some(active_target), Some(active_review_target)) =
                (active_target, active_review_target)
                && run.status == AutoReviewRunStatus::Completed
                && run.findings_suppressed_as_stale(active_target, active_review_target)
            {
                diagnostics.suppressed_stale_runs += 1;
            }
            if let Some(token_count) = run.token_count {
                diagnostics.token_count = diagnostics.token_count.saturating_add(token_count);
                diagnostics.token_runs += 1;
            }
            if let Some(prompt_token_estimate) = run.prompt_token_estimate {
                diagnostics.prompt_token_estimate = diagnostics
                    .prompt_token_estimate
                    .saturating_add(prompt_token_estimate);
                diagnostics.prompt_runs += 1;
            }
            if let Some(saved_token_estimate) = run.saved_token_estimate {
                diagnostics.saved_token_estimate = diagnostics
                    .saved_token_estimate
                    .saturating_add(saved_token_estimate);
                diagnostics.saved_runs += 1;
            }
            let elapsed_secs = run_elapsed_secs(run);
            let high_burn = run_has_high_burn_signal(run, elapsed_secs);
            if high_burn {
                diagnostics.high_burn_runs += 1;
            }
            let has_cost_signal = run.token_count.is_some()
                || run.prompt_token_estimate.is_some()
                || run.saved_token_estimate.is_some();
            if let Some(elapsed_secs) = elapsed_secs
                && (has_cost_signal || high_burn)
            {
                let bucket = duration_bucket(elapsed_secs);
                diagnostics.longest_elapsed_bucket =
                    Some(match diagnostics.longest_elapsed_bucket {
                        Some(existing)
                            if duration_bucket_rank(existing) >= duration_bucket_rank(bucket) =>
                        {
                            existing
                        }
                        _ => bucket,
                    });
            }
        }
        (diagnostics.recent_runs > 0).then_some(diagnostics)
    }

    pub fn compact_line(&self) -> String {
        let mut parts = vec![
            format!("recent_runs={}", self.recent_runs),
            format!("in_flight={}", self.in_flight_runs),
            format!("terminal={}", self.terminal_runs),
        ];
        push_nonzero(&mut parts, "suppressed_stale", self.suppressed_stale_runs);
        push_nonzero(&mut parts, "skipped", self.skipped_runs);
        push_nonzero(&mut parts, "duplicate_skipped", self.duplicate_skipped_runs);
        push_nonzero(&mut parts, "superseded", self.superseded_runs);
        push_nonzero(&mut parts, "failed", self.failed_runs);
        push_nonzero(&mut parts, "cancelled", self.cancelled_runs);
        push_nonzero(&mut parts, "lost", self.lost_runs);
        if self.token_runs > 0 {
            parts.push(format!("tokens={}t", self.token_count));
            parts.push(format!("token_runs={}", self.token_runs));
        }
        if self.prompt_runs > 0 {
            parts.push(format!("prompt_estimate={}t", self.prompt_token_estimate));
            parts.push(format!("prompt_runs={}", self.prompt_runs));
        }
        push_nonzero(&mut parts, "high_burn", self.high_burn_runs);
        if self.saved_runs > 0 {
            parts.push(format!("saved_estimate={}t", self.saved_token_estimate));
            parts.push(format!("saved_runs={}", self.saved_runs));
        }
        if let Some(longest_elapsed_bucket) = self.longest_elapsed_bucket {
            parts.push(format!("longest_elapsed={longest_elapsed_bucket}"));
        }
        parts.join(" ")
    }
}

impl AutoReviewLedgerProjection {
    pub fn from_runs<'a>(
        runs: impl IntoIterator<Item = &'a AutoReviewRun>,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> Self {
        let runs = runs.into_iter().collect::<Vec<_>>();
        let latest = runs
            .iter()
            .copied()
            .max_by_key(|run| run.sort_key())
            .map(|run| run.project(active_target, active_review_target));
        let current = runs
            .iter()
            .copied()
            .filter(|run| run.target_matches(active_target, active_review_target))
            .max_by_key(|run| run.sort_key())
            .map(|run| run.project(active_target, active_review_target));

        let mut status_counts = Vec::<AutoReviewStatusCount>::new();
        for run in &runs {
            let freshness = run.freshness(active_target);
            let target_matches = run.target_matches(active_target, active_review_target);
            if let Some(count) = status_counts.iter_mut().find(|count| {
                count.status == run.status
                    && count.source == run.source
                    && count.freshness == freshness
                    && count.target_matches == target_matches
            }) {
                count.count += 1;
            } else {
                status_counts.push(AutoReviewStatusCount {
                    status: run.status.clone(),
                    source: run.source.clone(),
                    freshness,
                    target_matches,
                    count: 1,
                });
            }
        }
        status_counts.sort_by(|left, right| {
            status_count_order_key(left).cmp(&status_count_order_key(right))
        });

        Self {
            latest,
            current,
            status_counts,
            diagnostics: AutoReviewDiagnostics::from_runs(
                runs.iter().copied(),
                Some(active_target),
                Some(active_review_target),
            ),
        }
    }
}

fn status_count_order_key(
    count: &AutoReviewStatusCount,
) -> (&'static str, &'static str, &'static str, &'static str) {
    (
        source_label(&count.source),
        status_label(&count.status),
        freshness_label(count.freshness),
        if count.target_matches {
            "target_current"
        } else {
            "off_target"
        },
    )
}

fn source_label(source: &AutoReviewRunSource) -> &'static str {
    match source {
        AutoReviewRunSource::Manual => "manual",
        AutoReviewRunSource::Background => "background",
    }
}

fn status_label(status: &AutoReviewRunStatus) -> &'static str {
    match status {
        AutoReviewRunStatus::Pending => "pending",
        AutoReviewRunStatus::Snapshotting => "snapshotting",
        AutoReviewRunStatus::Running => "running",
        AutoReviewRunStatus::Reviewing => "reviewing",
        AutoReviewRunStatus::Resolving => "resolving",
        AutoReviewRunStatus::Completed => "completed",
        AutoReviewRunStatus::Failed => "failed",
        AutoReviewRunStatus::Cancelled => "cancelled",
        AutoReviewRunStatus::Superseded => "superseded",
        AutoReviewRunStatus::Skipped => "skipped",
        AutoReviewRunStatus::Lost => "lost",
    }
}

fn freshness_label(freshness: AutoReviewFreshness) -> &'static str {
    match freshness {
        AutoReviewFreshness::Current => "current",
        AutoReviewFreshness::Stale => "stale",
        AutoReviewFreshness::Detached => "detached",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoReviewDuplicateDisposition {
    Adopt,
    ReuseTerminal,
    SupersedeTerminal,
}

impl AutoReviewStore {
    pub fn for_scope(codex_home: impl AsRef<Path>, scope: impl AsRef<Path>) -> Self {
        let codex_home = codex_home.as_ref();
        Self {
            root: scoped_store_root(codex_home, scope.as_ref()),
        }
    }

    pub fn from_store_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn has_store_files(codex_home: impl AsRef<Path>) -> bool {
        has_scoped_store_files(codex_home.as_ref())
    }

    pub fn save_run(&self, run: &AutoReviewRun) -> Result<PathBuf> {
        validate_run(run)?;
        let mut index = self.load_index_for_write()?;
        index.upsert(run.clone());
        let index = self.merged_compacted_index(index, run.run_id.as_str())?;
        self.save_run_metadata_index(&index)?;
        let path = self.save_index(index.clone())?;
        if let Err(err) = self.prune_run_metadata_except(&index) {
            tracing::warn!(
                error = %err,
                "failed to prune stale auto review run metadata"
            );
        }
        if let Err(err) = self.prune_run_states_except(&index) {
            tracing::warn!(
                error = %err,
                "failed to prune stale auto review run states"
            );
        }
        Ok(path)
    }

    fn merged_compacted_index(
        &self,
        mut index: AutoReviewRunsIndex,
        preferred_run_id: &str,
    ) -> Result<AutoReviewRunsIndex> {
        let runs_path = self.runs_path();
        if runs_path.exists() {
            let latest = load_runs_index_file(&runs_path)?;
            index.merge_latest_from_disk(latest, preferred_run_id);
        }
        index.compact_to_preserving(DEFAULT_MAX_RUNS, preferred_run_id);
        Ok(index)
    }

    fn save_index(&self, index: AutoReviewRunsIndex) -> Result<PathBuf> {
        let runs_path = self.runs_path();
        let json = serde_json::to_string_pretty(&index)?;
        write_atomically(&runs_path, &format!("{json}\n")).with_context(|| {
            format!(
                "failed to write auto review runs index {}",
                runs_path.display()
            )
        })?;
        Ok(runs_path)
    }

    pub fn mark_superseded(&self, run_id: &str, superseded_by: &str) -> Result<bool> {
        validate_safe_id(run_id).context("auto review run_id")?;
        validate_safe_id(superseded_by).context("auto review superseded_by")?;
        let mut index = self.load_index_for_write()?;
        let Some(run) = index.runs.iter_mut().find(|run| run.run_id == run_id) else {
            return Ok(false);
        };
        if run.run_id == superseded_by
            || run.status.is_in_flight()
            || run.status == AutoReviewRunStatus::Superseded
            || run.finding_count > 0
            || run.error_summary.is_some()
            || run.cancel_reason.is_some()
        {
            return Ok(false);
        }
        run.status = AutoReviewRunStatus::Superseded;
        run.freshness = AutoReviewRunFreshness::Superseded;
        run.completed_at_unix_secs = run
            .completed_at_unix_secs
            .or(Some(run.started_at_unix_secs));
        run.superseded_by = Some(superseded_by.to_string());
        self.save_run(&run.clone())?;
        Ok(true)
    }

    pub fn mark_superseded_by_fingerprint_with_target(
        &self,
        diff_fingerprint: &str,
        superseded_by: &str,
        active_branch: Option<&str>,
        active_head: Option<&str>,
        active_review_target: Option<&ReviewTarget>,
    ) -> Result<usize> {
        let fingerprint = diff_fingerprint.trim();
        if fingerprint.is_empty() {
            return Ok(0);
        }
        validate_safe_id(superseded_by).context("auto review superseded_by")?;
        let mut changed = 0;
        for run in self.load_index_for_write()?.runs {
            if run.run_id == superseded_by
                || auto_review_run_diff_fingerprint(&run) != Some(fingerprint)
                || !duplicate_target_matches_branch_head(&run, active_branch, active_head)
                || active_review_target
                    .is_some_and(|active_review_target| run.review_target != *active_review_target)
            {
                continue;
            }
            changed += usize::from(self.mark_superseded(&run.run_id, superseded_by)?);
        }
        Ok(changed)
    }

    pub fn reconcile_orphaned_in_flight<I>(
        &self,
        live_run_ids: I,
        now_unix_secs: i64,
    ) -> Result<usize>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let live_run_ids = live_run_ids
            .into_iter()
            .map(|run_id| run_id.as_ref().to_string())
            .collect::<BTreeSet<_>>();
        let mut index = self.load_index_for_write()?;
        let mut changed = Vec::new();
        for run in &mut index.runs {
            if !run.status.is_in_flight() {
                continue;
            }
            if live_run_ids.contains(&run.run_id) {
                continue;
            }
            run.status = AutoReviewRunStatus::Lost;
            run.freshness = AutoReviewRunFreshness::Lost;
            run.completed_at_unix_secs = Some(now_unix_secs);
            run.cancel_reason = Some("agent_missing_after_restart".to_string());
            changed.push(run.clone());
        }
        let changed_count = changed.len();
        for run in changed {
            self.save_run(&run)?;
        }
        Ok(changed_count)
    }

    pub fn find_duplicate_by_fingerprint_with_target_proof_and_filter<F>(
        &self,
        diff_fingerprint: &str,
        active_target: Option<&AutoReviewRunTarget>,
        active_review_target: Option<&ReviewTarget>,
        is_eligible: F,
    ) -> Result<Option<AutoReviewDuplicateMatch>>
    where
        F: Fn(&AutoReviewDuplicateMatch) -> bool,
    {
        let fingerprint = diff_fingerprint.trim();
        if fingerprint.is_empty() {
            return Ok(None);
        }
        Ok(self
            .load_index_for_read()?
            .runs
            .into_iter()
            .filter(|run| auto_review_run_diff_fingerprint(run) == Some(fingerprint))
            .filter(|run| {
                !matches!(
                    run.status,
                    AutoReviewRunStatus::Lost
                        | AutoReviewRunStatus::Skipped
                        | AutoReviewRunStatus::Superseded
                )
            })
            .filter(|run| duplicate_target_is_reusable(run, active_target))
            .filter(|run| {
                active_review_target
                    .is_none_or(|active_review_target| run.review_target == *active_review_target)
            })
            .filter_map(|run| {
                let duplicate = AutoReviewDuplicateMatch {
                    run_id: run.run_id.clone(),
                    status: run.status.clone(),
                    disposition: duplicate_disposition(&run),
                    finding_count: run.finding_count,
                    model: run.model.clone(),
                    token_count: run.token_count,
                    prompt_token_estimate: run.prompt_token_estimate,
                };
                is_eligible(&duplicate).then_some((run, duplicate))
            })
            .max_by(|left, right| {
                duplicate_priority(&left.0)
                    .cmp(&duplicate_priority(&right.0))
                    .then_with(|| {
                        auto_review_run_sort_key(&left.0).cmp(&auto_review_run_sort_key(&right.0))
                    })
            })
            .map(|(_run, duplicate)| duplicate))
    }

    pub fn load_run(&self, run_id: &str) -> Result<AutoReviewRun> {
        validate_safe_id(run_id).context("auto review run_id")?;
        let Some(run) = self
            .load_index_for_read()?
            .runs
            .into_iter()
            .find(|run| run.run_id == run_id)
        else {
            anyhow::bail!("unknown auto review run id: {run_id}");
        };
        validate_run(&run)?;
        Ok(run)
    }

    pub fn load_run_state(&self, run_id: &str) -> Result<Option<AutoReviewRunState>> {
        validate_safe_id(run_id).context("auto review run_id")?;
        self.load_run_state_unlocked(run_id)
    }

    pub fn save_run_state(&self, state: &AutoReviewRunState) -> Result<PathBuf> {
        let _guard = AUTO_REVIEW_RUN_STATE_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.save_run_state_unlocked(state)
    }

    pub fn update_run_state<F>(&self, run_id: &str, update: F) -> Result<AutoReviewRunState>
    where
        F: FnOnce(&mut AutoReviewRunState) -> Result<()>,
    {
        validate_safe_id(run_id).context("auto review run_id")?;
        let _guard = AUTO_REVIEW_RUN_STATE_WRITE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .load_run_state_unlocked(run_id)?
            .unwrap_or_else(|| AutoReviewRunState::new(run_id));
        update(&mut state)?;
        state.validate()?;
        self.save_run_state_unlocked(&state)?;
        Ok(state)
    }

    pub fn set_finding_disposition(
        &self,
        run_id: &str,
        disposition: AutoReviewFindingDispositionRecord,
    ) -> Result<AutoReviewRunState> {
        let run = self.load_run(run_id)?;
        if run.status != AutoReviewRunStatus::Completed {
            anyhow::bail!("auto review run is not completed: {run_id}");
        }
        if run.finding_count == 0 {
            anyhow::bail!("auto review run has no findings to disposition: {run_id}");
        }
        disposition.validate()?;
        self.update_run_state(run_id, |state| {
            state.finding_disposition = Some(disposition);
            Ok(())
        })
    }

    pub fn list_runs(&self) -> Result<Vec<AutoReviewRun>> {
        let mut runs = self.load_index_for_read()?.runs;
        runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        Ok(runs)
    }

    pub fn detail(
        &self,
        run_id: &str,
        finding_id: Option<&str>,
        max_bytes: usize,
    ) -> Result<AutoReviewDetail> {
        if max_bytes == 0 {
            anyhow::bail!("auto review detail max_bytes must be positive");
        }
        let run = self.load_run(run_id)?;
        if run.status != AutoReviewRunStatus::Completed {
            anyhow::bail!("auto review run is not completed: {run_id}");
        }
        let output = self.load_output(run_id)?;
        render_detail(finding_id, max_bytes, &output)
    }

    pub fn finding_detail(
        &self,
        run_id: &str,
        finding_id: &str,
        max_bytes: usize,
    ) -> Result<AutoReviewDetail> {
        self.detail(run_id, Some(finding_id), max_bytes)
    }

    pub fn output_path(&self, run_id: &str) -> Result<PathBuf> {
        validate_safe_id(run_id).context("auto review run_id")?;
        Ok(self.root.join(OUTPUTS_DIR).join(format!("{run_id}.json")))
    }

    pub fn runs_path(&self) -> PathBuf {
        self.root.join(RUNS_FILENAME)
    }

    fn load_index_strict(&self) -> Result<AutoReviewRunsIndex> {
        let path = self.runs_path();
        if !path.exists() {
            return Ok(AutoReviewRunsIndex::default());
        }
        load_runs_index_file(&path)
    }

    fn load_index_for_write(&self) -> Result<AutoReviewRunsIndex> {
        if self.runs_path().exists() {
            self.load_index_strict()
        } else {
            self.load_metadata_index_for_read()
        }
    }

    fn load_index_for_read(&self) -> Result<AutoReviewRunsIndex> {
        let path = self.runs_path();
        if !path.exists() {
            return self.load_metadata_index_for_read();
        }
        match load_runs_index_file(&path) {
            Ok(index) => Ok(index),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "auto review runs index is unreadable; recovering from run metadata"
                );
                self.load_metadata_index_for_read()
            }
        }
    }

    fn load_metadata_index_for_read(&self) -> Result<AutoReviewRunsIndex> {
        let mut index = AutoReviewRunsIndex::default();
        for run in self.load_metadata_runs() {
            index.upsert(run);
        }
        index.compact_to_preserving(DEFAULT_MAX_RUNS, "");
        index.validate()?;
        Ok(index)
    }

    fn run_metadata_path(&self, run_id: &str) -> Result<PathBuf> {
        validate_safe_id(run_id).context("auto review run_id")?;
        Ok(self
            .root
            .join(RUN_METADATA_DIR)
            .join(format!("{run_id}.json")))
    }

    fn run_state_path(&self, run_id: &str) -> Result<PathBuf> {
        validate_safe_id(run_id).context("auto review run_id")?;
        Ok(self
            .root
            .join(RUN_STATES_DIR)
            .join(format!("{run_id}.json")))
    }

    fn load_run_state_unlocked(&self, run_id: &str) -> Result<Option<AutoReviewRunState>> {
        let path = self.run_state_path(run_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let json = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read auto review run state {run_id}"))?;
        let state: AutoReviewRunState = serde_json::from_str(&json)
            .with_context(|| format!("failed to parse auto review run state {run_id}"))?;
        state.validate()?;
        Ok(Some(state))
    }

    fn save_run_state_unlocked(&self, state: &AutoReviewRunState) -> Result<PathBuf> {
        state.validate()?;
        let path = self.run_state_path(&state.run_id)?;
        let json = serde_json::to_string_pretty(state)?;
        write_atomically(&path, &format!("{json}\n"))
            .with_context(|| format!("failed to write auto review run state {}", path.display()))?;
        Ok(path)
    }

    fn save_run_metadata(&self, run: &AutoReviewRun) -> Result<()> {
        let path = self.run_metadata_path(&run.run_id)?;
        let json = serde_json::to_string_pretty(run)?;
        write_atomically(&path, &format!("{json}\n")).with_context(|| {
            format!(
                "failed to write auto review run metadata {}",
                path.display()
            )
        })?;
        Ok(())
    }

    fn save_run_metadata_index(&self, index: &AutoReviewRunsIndex) -> Result<()> {
        for run in &index.runs {
            self.save_run_metadata(run)?;
        }
        Ok(())
    }

    fn prune_run_metadata_except(&self, index: &AutoReviewRunsIndex) -> Result<()> {
        let metadata_dir = self.root.join(RUN_METADATA_DIR);
        if !metadata_dir.exists() {
            return Ok(());
        }
        let retained_run_ids = index
            .runs
            .iter()
            .map(|run| run.run_id.as_str())
            .collect::<BTreeSet<_>>();
        let entries = std::fs::read_dir(&metadata_dir).with_context(|| {
            format!(
                "failed to read auto review run metadata directory {}",
                metadata_dir.display()
            )
        })?;
        for entry in entries {
            let entry = entry.with_context(|| {
                format!(
                    "failed to read auto review run metadata directory {}",
                    metadata_dir.display()
                )
            })?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(run_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if validate_safe_id(run_id).is_ok() && !retained_run_ids.contains(run_id) {
                std::fs::remove_file(&path).with_context(|| {
                    format!(
                        "failed to remove auto review run metadata {}",
                        path.display()
                    )
                })?;
            }
        }
        Ok(())
    }

    fn prune_run_states_except(&self, index: &AutoReviewRunsIndex) -> Result<()> {
        let states_dir = self.root.join(RUN_STATES_DIR);
        if !states_dir.exists() {
            return Ok(());
        }
        let retained_run_ids = index
            .runs
            .iter()
            .map(|run| run.run_id.as_str())
            .collect::<BTreeSet<_>>();
        let entries = std::fs::read_dir(&states_dir).with_context(|| {
            format!(
                "failed to read auto review run states directory {}",
                states_dir.display()
            )
        })?;
        for entry in entries {
            let entry = entry.with_context(|| {
                format!(
                    "failed to read auto review run states directory {}",
                    states_dir.display()
                )
            })?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(run_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if validate_safe_id(run_id).is_ok() && !retained_run_ids.contains(run_id) {
                std::fs::remove_file(&path).with_context(|| {
                    format!("failed to remove auto review run state {}", path.display())
                })?;
            }
        }
        Ok(())
    }

    fn load_metadata_run(&self, run_id: &str) -> Result<AutoReviewRun> {
        let path = self.run_metadata_path(run_id)?;
        let json = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read auto review run metadata {run_id}"))?;
        let run: AutoReviewRun = serde_json::from_str(&json)
            .with_context(|| format!("failed to parse auto review run metadata {run_id}"))?;
        validate_run(&run)?;
        Ok(run)
    }

    fn load_metadata_runs(&self) -> Vec<AutoReviewRun> {
        let metadata_dir = self.root.join(RUN_METADATA_DIR);
        if !metadata_dir.exists() {
            return Vec::new();
        }

        let mut run_ids = Vec::new();
        let Ok(entries) = std::fs::read_dir(&metadata_dir) else {
            return Vec::new();
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(run_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if validate_safe_id(run_id).is_ok() {
                run_ids.push(run_id.to_string());
            }
        }

        run_ids.sort();
        run_ids
            .into_iter()
            .filter_map(|run_id| self.load_metadata_run(&run_id).ok())
            .collect()
    }

    pub fn save_output(&self, run_id: &str, output: &ReviewOutputEvent) -> Result<PathBuf> {
        let path = self.output_path(run_id)?;
        let json = serde_json::to_string_pretty(output)?;
        write_atomically(&path, &format!("{json}\n"))
            .with_context(|| format!("failed to write auto review output {}", path.display()))?;
        Ok(path)
    }

    fn load_output(&self, run_id: &str) -> Result<ReviewOutputEvent> {
        let path = self.output_path(run_id)?;
        let json = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read auto review output {run_id}"))?;
        serde_json::from_str(&json)
            .with_context(|| format!("failed to parse auto review output {run_id}"))
    }
}

fn load_runs_index_file(path: &Path) -> Result<AutoReviewRunsIndex> {
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read auto review runs index {}", path.display()))?;
    let parsed: AutoReviewRunsIndex = serde_json::from_str(&json)
        .with_context(|| format!("failed to parse auto review runs index {}", path.display()))?;
    parsed.validate()?;
    Ok(parsed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AutoReviewRunsIndex {
    schema_version: u32,
    runs: Vec<AutoReviewRun>,
}

impl Default for AutoReviewRunsIndex {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            runs: Vec::new(),
        }
    }
}

impl AutoReviewRunsIndex {
    fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported auto review runs index schema version: {}",
                self.schema_version
            );
        }
        let mut run_ids = BTreeSet::new();
        for run in &self.runs {
            validate_run(run)?;
            if !run_ids.insert(&run.run_id) {
                anyhow::bail!("duplicate auto review run id: {}", run.run_id);
            }
        }
        Ok(())
    }

    fn upsert(&mut self, run: AutoReviewRun) {
        let mut by_id = self
            .runs
            .drain(..)
            .map(|run| (run.run_id.clone(), run))
            .collect::<BTreeMap<_, _>>();
        by_id.insert(run.run_id.clone(), run);
        self.runs = by_id.into_values().collect();
    }

    fn merge_latest_from_disk(&mut self, latest: AutoReviewRunsIndex, preferred_run_id: &str) {
        let mut by_id = self
            .runs
            .drain(..)
            .map(|run| (run.run_id.clone(), run))
            .collect::<BTreeMap<_, _>>();
        for run in latest.runs {
            by_id
                .entry(run.run_id.clone())
                .and_modify(|existing| {
                    let is_preferred = preferred_run_id == existing.run_id.as_str();
                    if should_replace_merged_run(&run, existing, is_preferred) {
                        *existing = run.clone();
                    }
                })
                .or_insert(run);
        }
        self.runs = by_id.into_values().collect();
    }

    fn compact_to_preserving(&mut self, max_runs: usize, preferred_run_id: &str) {
        if self.runs.len() <= max_runs {
            return;
        }
        let preferred_run = self
            .runs
            .iter()
            .find(|run| run.run_id == preferred_run_id)
            .cloned();
        self.runs.sort_by(|left, right| {
            auto_review_run_sort_key(right).cmp(&auto_review_run_sort_key(left))
        });
        self.runs.truncate(max_runs);
        if let Some(preferred_run) = preferred_run
            && !self.runs.iter().any(|run| run.run_id == preferred_run_id)
        {
            let _evicted = self.runs.pop();
            self.runs.push(preferred_run);
        }
        self.runs
            .sort_by(|left, right| left.run_id.cmp(&right.run_id));
    }
}

fn should_replace_merged_run(
    candidate: &AutoReviewRun,
    existing: &AutoReviewRun,
    is_preferred: bool,
) -> bool {
    if is_preferred && existing.is_explicit_lifecycle_update() {
        return auto_review_run_sort_key(candidate) > auto_review_run_sort_key(existing);
    }
    if candidate.is_explicit_lifecycle_update() && !existing.is_explicit_lifecycle_update() {
        return auto_review_run_sort_key(candidate) >= auto_review_run_sort_key(existing);
    }
    run_is_newer(candidate, existing)
}

fn run_is_newer(candidate: &AutoReviewRun, existing: &AutoReviewRun) -> bool {
    if existing.status.is_terminal() != candidate.status.is_terminal() {
        return candidate.status.is_terminal();
    }
    let candidate_key = auto_review_run_sort_key(candidate);
    let existing_key = auto_review_run_sort_key(existing);
    if candidate_key != existing_key {
        return candidate_key > existing_key;
    }
    if existing.status.is_terminal()
        && candidate.status.is_terminal()
        && existing.status != candidate.status
    {
        return terminal_status_rank(&candidate.status) > terminal_status_rank(&existing.status);
    }
    status_progress_rank(&candidate.status) > status_progress_rank(&existing.status)
}

fn terminal_status_rank(status: &AutoReviewRunStatus) -> u8 {
    match status {
        AutoReviewRunStatus::Completed => 6,
        AutoReviewRunStatus::Failed => 5,
        AutoReviewRunStatus::Cancelled => 4,
        AutoReviewRunStatus::Lost => 3,
        AutoReviewRunStatus::Superseded => 2,
        AutoReviewRunStatus::Skipped => 1,
        AutoReviewRunStatus::Pending
        | AutoReviewRunStatus::Snapshotting
        | AutoReviewRunStatus::Running
        | AutoReviewRunStatus::Reviewing
        | AutoReviewRunStatus::Resolving => 0,
    }
}

fn status_progress_rank(status: &AutoReviewRunStatus) -> u8 {
    match status {
        AutoReviewRunStatus::Pending => 0,
        AutoReviewRunStatus::Snapshotting => 1,
        AutoReviewRunStatus::Running => 2,
        AutoReviewRunStatus::Reviewing => 3,
        AutoReviewRunStatus::Resolving => 4,
        AutoReviewRunStatus::Completed
        | AutoReviewRunStatus::Failed
        | AutoReviewRunStatus::Cancelled
        | AutoReviewRunStatus::Superseded
        | AutoReviewRunStatus::Skipped
        | AutoReviewRunStatus::Lost => 5,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewRun {
    pub schema_version: u32,
    pub run_id: String,
    pub status: AutoReviewRunStatus,
    pub freshness: AutoReviewRunFreshness,
    pub source: AutoReviewRunSource,
    pub target: AutoReviewRunTarget,
    pub review_target: ReviewTarget,
    pub started_at_unix_secs: i64,
    pub completed_at_unix_secs: Option<i64>,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_token_estimate: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_token_estimate: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
    pub error_summary: Option<String>,
    pub finding_count: usize,
    pub finding_digests: Vec<AutoReviewFindingDigest>,
    pub omitted_finding_digest_count: usize,
}

pub const RUN_STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewRunState {
    pub schema_version: u32,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<AutoReviewBudget>,
    #[serde(default)]
    pub usage: AutoReviewUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<AutoReviewTerminalReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_disposition: Option<AutoReviewFindingDispositionRecord>,
}

impl AutoReviewRunState {
    pub fn new(run_id: impl Into<String>) -> Self {
        Self {
            schema_version: RUN_STATE_SCHEMA_VERSION,
            run_id: run_id.into(),
            budget: None,
            usage: AutoReviewUsage::default(),
            terminal_reason: None,
            finding_disposition: None,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != RUN_STATE_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported auto review run state schema version: {}",
                self.schema_version
            );
        }
        validate_safe_id(&self.run_id).context("auto review run state run_id")?;
        if let Some(budget) = &self.budget {
            budget.validate()?;
        }
        if let Some(disposition) = &self.finding_disposition {
            disposition.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewBudget {
    pub max_scope_bytes: usize,
    pub max_elapsed_ms: u64,
    pub max_total_tokens: u64,
    pub max_output_bytes: usize,
    pub max_findings: usize,
}

impl AutoReviewBudget {
    pub fn validate(&self) -> Result<()> {
        if self.max_scope_bytes == 0
            || self.max_elapsed_ms == 0
            || self.max_total_tokens == 0
            || self.max_output_bytes == 0
            || self.max_findings == 0
        {
            anyhow::bail!("auto review budget limits must all be positive");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding_count: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewTerminalReason {
    BudgetScope,
    BudgetElapsed,
    BudgetTotalTokens,
    BudgetOutput,
    BudgetFindingCount,
    EmptyOutput,
    StaleTarget,
}

impl AutoReviewTerminalReason {
    pub const fn cancel_reason(self) -> &'static str {
        match self {
            Self::BudgetScope => "budget_scope",
            Self::BudgetElapsed => "budget_elapsed",
            Self::BudgetTotalTokens => "budget_total_tokens",
            Self::BudgetOutput => "budget_output",
            Self::BudgetFindingCount => "budget_finding_count",
            Self::EmptyOutput => "empty_output",
            Self::StaleTarget => "stale_target",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewFindingDisposition {
    NeedsAttention,
    Repairing,
    Deferred,
    Obsolete,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewDispositionActor {
    User,
    Agent,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewFindingDispositionRecord {
    pub disposition: AutoReviewFindingDisposition,
    pub actor: AutoReviewDispositionActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub updated_at_unix_secs: i64,
}

impl AutoReviewFindingDispositionRecord {
    fn validate(&self) -> Result<()> {
        if matches!(self.disposition, AutoReviewFindingDisposition::Obsolete)
            && self
                .reason
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            anyhow::bail!("obsolete auto review disposition requires a reason");
        }
        if self
            .reason
            .as_deref()
            .is_some_and(|reason| reason.len() > SUMMARY_MAX_FIELD_BYTES)
        {
            anyhow::bail!("auto review disposition reason exceeds {SUMMARY_MAX_FIELD_BYTES} bytes");
        }
        Ok(())
    }
}

impl AutoReviewRun {
    pub fn sort_key(&self) -> (i64, &str) {
        (
            self.completed_at_unix_secs
                .unwrap_or(self.started_at_unix_secs),
            self.run_id.as_str(),
        )
    }

    pub fn target_matches(
        &self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> bool {
        self.freshness(active_target) == AutoReviewFreshness::Current
            && review_target_matches(&self.review_target, active_review_target)
    }

    pub fn project(
        &self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> AutoReviewRunProjection {
        AutoReviewRunProjection {
            run_id: self.run_id.clone(),
            status: self.status.clone(),
            source: self.source.clone(),
            freshness: self.freshness(active_target),
            target_matches: self.target_matches(active_target, active_review_target),
            started_at_unix_secs: self.started_at_unix_secs,
            completed_at_unix_secs: self.completed_at_unix_secs,
            model: self.model.clone(),
            error_summary: self.error_summary.clone(),
            summary: self.summary(active_target, active_review_target),
        }
    }

    pub fn visible_finding_digests<'a>(
        &'a self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> Vec<&'a AutoReviewFindingDigest> {
        if self.status != AutoReviewRunStatus::Completed {
            return Vec::new();
        }
        if !review_target_matches(&self.review_target, active_review_target) {
            return Vec::new();
        }
        if !self.is_current_for(active_target, active_review_target) {
            return Vec::new();
        }
        self.finding_digests.iter().collect()
    }

    fn findings_suppressed_as_stale(
        &self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> bool {
        self.status == AutoReviewRunStatus::Completed
            && review_target_matches(&self.review_target, active_review_target)
            && !self.is_current_for(active_target, active_review_target)
    }

    pub fn can_read_finding_detail(
        &self,
        finding_id: &str,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> bool {
        if !self.can_read_detail(active_target, active_review_target) {
            return false;
        }
        parse_finding_id(finding_id).is_some_and(|index| index < self.finding_count)
    }

    pub fn can_read_detail(
        &self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> bool {
        self.status == AutoReviewRunStatus::Completed
            && review_target_matches(&self.review_target, active_review_target)
            && self.is_current_for(active_target, active_review_target)
    }

    pub fn freshness(&self, active_target: &AutoReviewRunTarget) -> AutoReviewFreshness {
        if matches!(
            self.freshness,
            AutoReviewRunFreshness::Lost
                | AutoReviewRunFreshness::Superseded
                | AutoReviewRunFreshness::Obsolete
        ) {
            return AutoReviewFreshness::Stale;
        }
        self.target.freshness(active_target)
    }

    fn is_explicit_lifecycle_update(&self) -> bool {
        self.status == AutoReviewRunStatus::Superseded
            || self.superseded_by.is_some()
            || self.cancel_reason.is_some()
    }

    fn is_current_for(
        &self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> bool {
        match active_review_target {
            ReviewTarget::Commit { sha, .. } => active_target.head_sha.as_deref() == Some(sha),
            _ => self.freshness(active_target) == AutoReviewFreshness::Current,
        }
    }

    pub fn summary(
        &self,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> AutoReviewSummary {
        render_summary(
            self.visible_finding_digests(active_target, active_review_target),
            self.omitted_finding_digest_count,
        )
    }

    pub fn is_duplicate_skipped(&self) -> bool {
        self.status == AutoReviewRunStatus::Skipped
            && self.cancel_reason.as_deref() == Some(DUPLICATE_AUTO_REVIEW_SCOPE_CANCEL_REASON)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewRunStatus {
    Pending,
    Snapshotting,
    Running,
    Reviewing,
    Resolving,
    Completed,
    Failed,
    Cancelled,
    Superseded,
    Skipped,
    Lost,
}

impl AutoReviewRunStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Superseded
                | Self::Skipped
                | Self::Lost
        )
    }

    pub fn is_in_flight(&self) -> bool {
        !self.is_terminal()
    }

    pub fn is_adoptable_duplicate(&self) -> bool {
        matches!(self, Self::Running | Self::Reviewing | Self::Resolving)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewRunFreshness {
    Current,
    LongRunning,
    Inactive,
    Superseded,
    Obsolete,
    Lost,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewRunSource {
    Manual,
    Background,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewRunTarget {
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
    pub worktree_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_epoch: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_at_launch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_diff_fingerprint: Option<String>,
}

impl AutoReviewRunTarget {
    pub fn freshness(&self, active: &Self) -> AutoReviewFreshness {
        if self.branch != active.branch || self.worktree_path != active.worktree_path {
            return AutoReviewFreshness::Detached;
        }
        if self.head_sha != active.head_sha {
            return AutoReviewFreshness::Stale;
        }
        if self.base_sha != active.base_sha {
            return AutoReviewFreshness::Stale;
        }
        if active.snapshot_epoch.is_some() && self.snapshot_epoch != active.snapshot_epoch {
            return AutoReviewFreshness::Stale;
        }
        if self.snapshot_commit.is_some()
            && active.snapshot_commit.is_some()
            && self.snapshot_commit != active.snapshot_commit
        {
            return AutoReviewFreshness::Stale;
        }
        if self.worktree_diff_fingerprint != active.worktree_diff_fingerprint {
            return AutoReviewFreshness::Stale;
        }
        AutoReviewFreshness::Current
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoReviewFreshness {
    Current,
    Stale,
    Detached,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct AutoReviewFindingDigest {
    pub finding_id: String,
    pub priority: i32,
    pub title: String,
    pub path: Option<PathBuf>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
}

impl AutoReviewFindingDigest {
    fn summary_line(&self) -> String {
        let priority = self.priority.to_string();
        let title = truncate_utf8(&self.title, SUMMARY_MAX_FIELD_BYTES);
        let location = self
            .path
            .as_ref()
            .map(|path| match (self.line_start, self.line_end) {
                (Some(start), Some(end)) => format!("{}:{start}-{end}", path.display()),
                (Some(start), None) => format!("{}:{start}", path.display()),
                _ => path.display().to_string(),
            })
            .unwrap_or_else(|| "unknown location".to_string());
        let location = truncate_utf8(&location, SUMMARY_MAX_FIELD_BYTES);
        let finding_id = truncate_utf8(&self.finding_id, 80);
        format!("[P{priority}] {finding_id}: {title} ({location})")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReviewSummary {
    pub content: String,
    pub rendered_findings: usize,
    pub omitted_findings: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoReviewDetailKind {
    Run,
    Finding,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReviewDetail {
    pub kind: AutoReviewDetailKind,
    pub finding_id: Option<String>,
    pub finding_count: usize,
    pub omitted_findings: usize,
    pub bytes: usize,
    pub original_bytes: usize,
    pub max_bytes: usize,
    pub truncated: bool,
    pub content: String,
}

fn review_target_matches(stored: &ReviewTarget, active: &ReviewTarget) -> bool {
    match (stored, active) {
        (
            ReviewTarget::Commit {
                sha: stored_sha, ..
            },
            ReviewTarget::Commit {
                sha: active_sha, ..
            },
        ) => stored_sha == active_sha,
        (ReviewTarget::CurrentTurnDiff { .. }, ReviewTarget::UncommittedChanges)
        | (ReviewTarget::UncommittedChanges, ReviewTarget::CurrentTurnDiff { .. }) => true,
        _ => stored == active,
    }
}

fn duplicate_priority(run: &AutoReviewRun) -> u8 {
    if run.status.is_adoptable_duplicate() {
        return 4;
    }
    if run.finding_count > 0 {
        return 3;
    }
    if run.status == AutoReviewRunStatus::Completed {
        return 2;
    }
    1
}

fn duplicate_disposition(run: &AutoReviewRun) -> AutoReviewDuplicateDisposition {
    if run.status.is_adoptable_duplicate() {
        AutoReviewDuplicateDisposition::Adopt
    } else if run.status == AutoReviewRunStatus::Completed {
        AutoReviewDuplicateDisposition::ReuseTerminal
    } else {
        AutoReviewDuplicateDisposition::SupersedeTerminal
    }
}

fn push_nonzero(parts: &mut Vec<String>, label: &str, count: usize) {
    if count > 0 {
        parts.push(format!("{label}={count}"));
    }
}

fn duplicate_target_matches_branch_head(
    run: &AutoReviewRun,
    active_branch: Option<&str>,
    active_head: Option<&str>,
) -> bool {
    if active_head.and_then(non_empty_str).is_none() {
        return true;
    }
    if let Some(active_branch) = active_branch.and_then(non_empty_str)
        && run.target.branch.as_deref() != Some(active_branch)
    {
        return false;
    }
    run.target.head_sha.as_deref() == active_head.and_then(non_empty_str)
}

fn auto_review_run_diff_fingerprint(run: &AutoReviewRun) -> Option<&str> {
    run.target
        .worktree_diff_fingerprint
        .as_deref()
        .and_then(non_empty_str)
        .or_else(|| match &run.review_target {
            ReviewTarget::CurrentTurnDiff { fingerprint } => non_empty_str(fingerprint),
            _ => None,
        })
}

fn duplicate_target_is_reusable(
    run: &AutoReviewRun,
    active_target: Option<&AutoReviewRunTarget>,
) -> bool {
    active_target.is_none_or(|active_target| {
        run.target.freshness(active_target) == AutoReviewFreshness::Current
    })
}

fn non_empty_str(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn run_elapsed_secs(run: &AutoReviewRun) -> Option<i64> {
    run.completed_at_unix_secs
        .map(|completed_at| (completed_at - run.started_at_unix_secs).max(0))
}

fn run_has_high_burn_signal(run: &AutoReviewRun, elapsed_secs: Option<i64>) -> bool {
    run.token_count
        .is_some_and(|token_count| token_count >= LEDGER_HIGH_TOKEN_COUNT)
        || run
            .prompt_token_estimate
            .is_some_and(|estimate| estimate >= LEDGER_HIGH_PROMPT_TOKEN_ESTIMATE)
        || elapsed_secs.is_some_and(|elapsed| elapsed >= LEDGER_LONG_ELAPSED_SECS)
}

fn duration_bucket(seconds: i64) -> &'static str {
    match seconds {
        0..=59 => "lt1m",
        60..=299 => "lt5m",
        300..=899 => "lt15m",
        900..=3599 => "lt1h",
        _ => "gte1h",
    }
}

fn duration_bucket_rank(bucket: &str) -> u8 {
    match bucket {
        "lt1m" => 0,
        "lt5m" => 1,
        "lt15m" => 2,
        "lt1h" => 3,
        "gte1h" => 4,
        _ => 0,
    }
}

fn auto_review_run_sort_key(run: &AutoReviewRun) -> (i64, &str) {
    (
        run.completed_at_unix_secs
            .unwrap_or(run.started_at_unix_secs),
        run.run_id.as_str(),
    )
}

pub fn finding_digests(output: &ReviewOutputEvent) -> Vec<AutoReviewFindingDigest> {
    output
        .findings
        .iter()
        .take(SUMMARY_MAX_FINDINGS)
        .enumerate()
        .map(|(index, finding)| AutoReviewFindingDigest {
            finding_id: format!("f{}", index + 1),
            priority: finding.priority,
            title: truncate_utf8(&finding.title, SUMMARY_MAX_FIELD_BYTES),
            path: Some(finding.code_location.absolute_file_path.clone()),
            line_start: Some(finding.code_location.line_range.start),
            line_end: Some(finding.code_location.line_range.end),
        })
        .collect()
}

fn render_detail(
    finding_id: Option<&str>,
    max_bytes: usize,
    output: &ReviewOutputEvent,
) -> Result<AutoReviewDetail> {
    let effective_max_bytes = max_bytes.min(DETAIL_MAX_BYTES);
    let (kind, content, omitted_findings, findings_capped) = match finding_id {
        Some(finding_id) => {
            let finding_index = parse_finding_id(finding_id)
                .with_context(|| format!("invalid auto review finding id: {finding_id}"))?;
            let finding = output
                .findings
                .get(finding_index)
                .with_context(|| format!("unknown auto review finding id: {finding_id}"))?;
            (
                AutoReviewDetailKind::Finding,
                format_finding_detail(finding_id, finding),
                output.findings.len().saturating_sub(1),
                false,
            )
        }
        None => (
            AutoReviewDetailKind::Run,
            format_run_detail(output),
            output.findings.len().saturating_sub(DETAIL_MAX_FINDINGS),
            output.findings.len() > DETAIL_MAX_FINDINGS,
        ),
    };
    let original_bytes = content.len();
    let (content, truncated_by_bytes) = truncate_utf8_with_marker(&content, effective_max_bytes);
    let bytes = content.len();

    Ok(AutoReviewDetail {
        kind,
        finding_id: finding_id.map(str::to_string),
        finding_count: output.findings.len(),
        omitted_findings,
        bytes,
        original_bytes,
        max_bytes: effective_max_bytes,
        truncated: truncated_by_bytes || findings_capped,
        content,
    })
}

fn format_run_detail(output: &ReviewOutputEvent) -> String {
    let mut sections = vec![format!(
        "overall_correctness: {}\noverall_confidence: {}\noverall_explanation:\n{}",
        output.overall_correctness.trim(),
        output.overall_confidence_score,
        output.overall_explanation.trim()
    )];
    if !output.findings.is_empty() {
        let mut findings = String::new();
        for (index, finding) in output.findings.iter().take(DETAIL_MAX_FINDINGS).enumerate() {
            if !findings.is_empty() {
                findings.push_str("\n\n");
            }
            findings.push_str(&format_finding_detail(&format!("f{}", index + 1), finding));
        }
        if output.findings.len() > DETAIL_MAX_FINDINGS {
            findings.push_str(&format!(
                "\n... omitted {} additional finding(s); request a specific findingId for full detail",
                output.findings.len() - DETAIL_MAX_FINDINGS
            ));
        }
        sections.push(findings);
    }
    sections.join("\n\n")
}

fn format_finding_detail(
    finding_id: &str,
    finding: &codex_protocol::protocol::ReviewFinding,
) -> String {
    format!(
        "finding_id={finding_id} priority={} confidence={} location={}:{}-{}\ntitle: {}\nbody:\n{}",
        finding.priority,
        finding.confidence_score,
        finding.code_location.absolute_file_path.display(),
        finding.code_location.line_range.start,
        finding.code_location.line_range.end,
        finding.title.trim(),
        finding.body.trim()
    )
}

fn parse_finding_id(finding_id: &str) -> Option<usize> {
    finding_id
        .strip_prefix('f')?
        .parse::<usize>()
        .ok()?
        .checked_sub(1)
}

fn render_summary(
    findings: Vec<&AutoReviewFindingDigest>,
    stored_omitted_findings: usize,
) -> AutoReviewSummary {
    if findings.is_empty() {
        return AutoReviewSummary {
            content: String::new(),
            rendered_findings: 0,
            omitted_findings: stored_omitted_findings,
            truncated: false,
        };
    }

    let mut lines = Vec::new();
    let max_rendered_findings = findings.len().min(SUMMARY_MAX_FINDINGS);
    let mut truncated = false;

    for finding in findings.iter().take(max_rendered_findings) {
        let line = finding.summary_line();
        let omitted_after_candidate = findings.len() - (lines.len() + 1);
        let reserved_bytes = if omitted_after_candidate > 0 {
            omitted_line(omitted_after_candidate).len() + 1
        } else {
            0
        };
        let remaining_bytes = SUMMARY_MAX_BYTES.saturating_sub(reserved_bytes);
        let mut candidate_lines = lines.clone();
        candidate_lines.push(line);
        let candidate = candidate_lines.join("\n");
        if candidate.len() > remaining_bytes {
            truncated = true;
            break;
        }
        lines = candidate_lines;
    }

    let omitted_findings = findings.len() - lines.len() + stored_omitted_findings;
    if omitted_findings > 0 {
        lines.push(omitted_line(omitted_findings));
    }
    let content = truncate_utf8(&lines.join("\n"), SUMMARY_MAX_BYTES);

    AutoReviewSummary {
        content,
        rendered_findings: lines
            .len()
            .saturating_sub(usize::from(omitted_findings > 0)),
        omitted_findings,
        truncated,
    }
}

fn omitted_line(count: usize) -> String {
    format!("{OMITTED_TEMPLATE_PREFIX}{count}{OMITTED_TEMPLATE_SUFFIX}")
}

fn validate_safe_id(value: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!("must be a non-empty string");
    }
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        && !value.trim_matches(['.', '-']).is_empty()
    {
        return Ok(());
    }
    anyhow::bail!("contains unsafe path characters: {value:?}")
}

fn scoped_store_root(codex_home: &Path, scope: &Path) -> PathBuf {
    scoped_review_root(codex_home, scope).join(STORE_DIR)
}

fn scoped_review_root(codex_home: &Path, scope: &Path) -> PathBuf {
    codex_home
        .join(STATE_DIR)
        .join(REVIEW_DIR)
        .join(repo_key(scope))
}

fn has_scoped_store_files(codex_home: &Path) -> bool {
    let review_dir = codex_home.join(STATE_DIR).join(REVIEW_DIR);
    let Ok(entries) = std::fs::read_dir(&review_dir) else {
        return false;
    };

    for entry in entries.flatten() {
        let store_root = entry.path().join(STORE_DIR);
        if store_root.join(RUNS_FILENAME).exists()
            || has_json_file(&store_root.join(RUN_METADATA_DIR))
            || has_json_file(&store_root.join(OUTPUTS_DIR))
        {
            return true;
        }
    }
    false
}

fn has_json_file(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
}

fn repo_key(scope: &Path) -> String {
    let normalized_scope = scope.canonicalize().unwrap_or_else(|_| scope.to_path_buf());
    let key = crc32fast::hash(normalized_scope.to_string_lossy().as_bytes());
    format!("repo-{key:08x}")
}

fn validate_run(run: &AutoReviewRun) -> Result<()> {
    if run.schema_version != SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported auto review schema version: {}",
            run.schema_version
        );
    }
    validate_safe_id(&run.run_id).context("auto review run_id")?;

    if run.finding_digests.len() > run.finding_count {
        anyhow::bail!(
            "auto review run {} has more finding digests than findings",
            run.run_id
        );
    }
    if run.finding_digests.len() > SUMMARY_MAX_FINDINGS {
        anyhow::bail!(
            "auto review run {} stores too many finding digests: {} > {}",
            run.run_id,
            run.finding_digests.len(),
            SUMMARY_MAX_FINDINGS,
        );
    }
    let expected_omitted = run.finding_count.saturating_sub(run.finding_digests.len());
    if run.omitted_finding_digest_count != expected_omitted {
        anyhow::bail!(
            "auto review run {} has inconsistent omitted finding digest count: {} != {}",
            run.run_id,
            run.omitted_finding_digest_count,
            expected_omitted,
        );
    }
    let mut finding_ids = BTreeSet::new();
    for (index, finding) in run.finding_digests.iter().enumerate() {
        let expected_id = format!("f{}", index + 1);
        if finding.finding_id != expected_id {
            anyhow::bail!(
                "auto review run {} has non-canonical finding id: {} != {}",
                run.run_id,
                finding.finding_id,
                expected_id,
            );
        }
        validate_safe_id(&finding.finding_id).context("auto review finding_id")?;
        if !finding_ids.insert(&finding.finding_id) {
            anyhow::bail!("duplicate auto review finding id: {}", finding.finding_id);
        }
    }
    Ok(())
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn truncate_utf8_with_marker(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    (truncate_utf8(value, max_bytes), true)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
