use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

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
pub const SCHEMA_VERSION: u32 = 1;

const STORE_DIR: &str = "auto-review";
const STATE_DIR: &str = "state";
const REVIEW_DIR: &str = "review";
const RUNS_FILENAME: &str = "runs.json";
const OUTPUTS_DIR: &str = "outputs";
const OMITTED_TEMPLATE_PREFIX: &str = "... ";
const OMITTED_TEMPLATE_SUFFIX: &str = " more finding(s) omitted";

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
        let mut index = self.load_index_for_read()?;
        index.upsert(run.clone());
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
        let mut index = self.load_index_for_read()?;
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

    pub fn mark_superseded_by_fingerprint(
        &self,
        diff_fingerprint: &str,
        superseded_by: &str,
    ) -> Result<usize> {
        self.mark_superseded_by_fingerprint_with_target(
            diff_fingerprint,
            superseded_by,
            /*active_branch*/ None,
            /*active_head*/ None,
        )
    }

    pub fn mark_superseded_by_fingerprint_with_target(
        &self,
        diff_fingerprint: &str,
        superseded_by: &str,
        active_branch: Option<&str>,
        active_head: Option<&str>,
    ) -> Result<usize> {
        let fingerprint = diff_fingerprint.trim();
        if fingerprint.is_empty() {
            return Ok(0);
        }
        validate_safe_id(superseded_by).context("auto review superseded_by")?;
        let mut changed = 0;
        for run in self.load_index_for_read()?.runs {
            if run.run_id == superseded_by
                || run.target.worktree_diff_fingerprint.as_deref() != Some(fingerprint)
                || !duplicate_target_matches_branch_head(&run, active_branch, active_head)
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
        self.reconcile_orphaned_in_flight_matching(live_run_ids, now_unix_secs, |_| true)
    }

    pub fn reconcile_orphaned_background_in_flight<I>(
        &self,
        live_run_ids: I,
        now_unix_secs: i64,
    ) -> Result<usize>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.reconcile_orphaned_in_flight_matching(live_run_ids, now_unix_secs, |run| {
            run.source == AutoReviewRunSource::Background
        })
    }

    fn reconcile_orphaned_in_flight_matching<I, F>(
        &self,
        live_run_ids: I,
        now_unix_secs: i64,
        should_reconcile: F,
    ) -> Result<usize>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
        F: Fn(&AutoReviewRun) -> bool,
    {
        let live_run_ids = live_run_ids
            .into_iter()
            .map(|run_id| run_id.as_ref().to_string())
            .collect::<BTreeSet<_>>();
        let mut index = self.load_index_for_read()?;
        let mut changed = Vec::new();
        for run in &mut index.runs {
            if !run.status.is_in_flight() {
                continue;
            }
            if !should_reconcile(run) {
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

    pub fn find_duplicate_by_fingerprint(
        &self,
        diff_fingerprint: &str,
    ) -> Result<Option<AutoReviewDuplicateMatch>> {
        self.find_duplicate_by_fingerprint_with_target(
            diff_fingerprint,
            /*active_branch*/ None,
            /*active_head*/ None,
        )
    }

    pub fn find_duplicate_by_fingerprint_with_target(
        &self,
        diff_fingerprint: &str,
        active_branch: Option<&str>,
        active_head: Option<&str>,
    ) -> Result<Option<AutoReviewDuplicateMatch>> {
        self.find_duplicate_by_fingerprint_with_target_and_filter(
            diff_fingerprint,
            active_branch,
            active_head,
            |_| true,
        )
    }

    pub fn find_duplicate_by_fingerprint_with_target_and_filter<F>(
        &self,
        diff_fingerprint: &str,
        active_branch: Option<&str>,
        active_head: Option<&str>,
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
            .filter(|run| run.target.worktree_diff_fingerprint.as_deref() == Some(fingerprint))
            .filter(|run| {
                !matches!(
                    run.status,
                    AutoReviewRunStatus::Lost
                        | AutoReviewRunStatus::Skipped
                        | AutoReviewRunStatus::Superseded
                )
            })
            .filter(|run| duplicate_target_matches_branch_head(run, active_branch, active_head))
            .filter_map(|run| {
                let duplicate = AutoReviewDuplicateMatch {
                    run_id: run.run_id.clone(),
                    status: run.status.clone(),
                    disposition: duplicate_disposition(&run),
                    finding_count: run.finding_count,
                    model: run.model.clone(),
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

    pub fn find_duplicate_by_fingerprint_with_target_proof_and_filter<F>(
        &self,
        diff_fingerprint: &str,
        active_target: Option<&AutoReviewRunTarget>,
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
            .filter(|run| run.target.worktree_diff_fingerprint.as_deref() == Some(fingerprint))
            .filter(|run| {
                !matches!(
                    run.status,
                    AutoReviewRunStatus::Lost
                        | AutoReviewRunStatus::Skipped
                        | AutoReviewRunStatus::Superseded
                )
            })
            .filter(|run| duplicate_target_is_reusable(run, active_target))
            .filter_map(|run| {
                let duplicate = AutoReviewDuplicateMatch {
                    run_id: run.run_id.clone(),
                    status: run.status.clone(),
                    disposition: duplicate_disposition(&run),
                    finding_count: run.finding_count,
                    model: run.model.clone(),
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

    pub fn list_runs(&self) -> Result<Vec<AutoReviewRun>> {
        let mut runs = self.load_index_for_read()?.runs;
        runs.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        Ok(runs)
    }

    pub fn finding_detail(
        &self,
        run_id: &str,
        finding_id: &str,
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
        render_finding_detail(finding_id, max_bytes, &output)
    }

    pub fn output_path(&self, run_id: &str) -> Result<PathBuf> {
        validate_safe_id(run_id).context("auto review run_id")?;
        Ok(self.root.join(OUTPUTS_DIR).join(format!("{run_id}.json")))
    }

    pub fn runs_path(&self) -> PathBuf {
        self.root.join(RUNS_FILENAME)
    }

    fn load_index_for_read(&self) -> Result<AutoReviewRunsIndex> {
        let path = self.runs_path();
        if !path.exists() {
            return Ok(AutoReviewRunsIndex::default());
        }
        load_runs_index_file(&path)
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
    pub superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
    pub error_summary: Option<String>,
    pub finding_count: usize,
    pub finding_digests: Vec<AutoReviewFindingDigest>,
    pub omitted_finding_digest_count: usize,
}

impl AutoReviewRun {
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

    pub fn can_read_finding_detail(
        &self,
        finding_id: &str,
        active_target: &AutoReviewRunTarget,
        active_review_target: &ReviewTarget,
    ) -> bool {
        if self.status != AutoReviewRunStatus::Completed {
            return false;
        }
        if !review_target_matches(&self.review_target, active_review_target) {
            return false;
        }
        if !self.is_current_for(active_target, active_review_target) {
            return false;
        }
        parse_finding_id(finding_id).is_some_and(|index| index < self.finding_count)
    }

    pub fn freshness(&self, active_target: &AutoReviewRunTarget) -> AutoReviewFreshness {
        if self.freshness == AutoReviewRunFreshness::Lost {
            return AutoReviewFreshness::Stale;
        }
        if self.freshness == AutoReviewRunFreshness::Superseded {
            return AutoReviewFreshness::Stale;
        }
        self.target.freshness(active_target)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReviewDetail {
    pub finding_id: String,
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

fn render_finding_detail(
    finding_id: &str,
    max_bytes: usize,
    output: &ReviewOutputEvent,
) -> Result<AutoReviewDetail> {
    let finding_index = parse_finding_id(finding_id)
        .with_context(|| format!("invalid auto review finding id: {finding_id}"))?;
    let finding = output
        .findings
        .get(finding_index)
        .with_context(|| format!("unknown auto review finding id: {finding_id}"))?;
    let effective_max_bytes = max_bytes.min(DETAIL_MAX_BYTES);
    let json = serde_json::to_string_pretty(finding)?;
    let original_bytes = json.len();
    let (content, truncated) = truncate_utf8_with_marker(&json, effective_max_bytes);
    let bytes = content.len();

    Ok(AutoReviewDetail {
        finding_id: finding_id.to_string(),
        bytes,
        original_bytes,
        max_bytes: effective_max_bytes,
        truncated,
        content,
    })
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
        if store_root.join(RUNS_FILENAME).exists() || has_json_file(&store_root.join(OUTPUTS_DIR)) {
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
