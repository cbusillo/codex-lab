use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use codex_auto_review::AutoReviewBudget;
use codex_auto_review::AutoReviewDispositionActor;
use codex_auto_review::AutoReviewDuplicateMatch;
use codex_auto_review::AutoReviewFindingDisposition;
use codex_auto_review::AutoReviewFindingDispositionRecord;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunFreshness;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::AutoReviewTerminalReason;
use codex_auto_review::AutoReviewUsage;
use codex_auto_review::ReviewCoordination;
use codex_auto_review::SCHEMA_VERSION;
use codex_auto_review::finding_digests;
use codex_git_utils::collect_git_info;
use codex_git_utils::get_git_repo_root;
use codex_git_utils::get_worktree_diff_fingerprint;
use codex_git_utils::merge_base_with_head;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPersistence;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::TokenUsage;

use crate::turn_timing::now_unix_timestamp_ms;

static AUTO_REVIEW_RUN_WRITE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) const AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY: &str =
    "review was interrupted before producing structured output";

#[derive(Clone, Debug)]
pub(crate) struct ReviewPersistenceContext {
    run_id: String,
    source: AutoReviewRunSource,
    store_scope: PathBuf,
    target: AutoReviewRunTarget,
    review_target: ReviewTarget,
    started_at_unix_secs: i64,
    model: Option<String>,
    reasoning_effort: Option<String>,
    prompt_token_estimate: Option<u64>,
    background_budget: Option<AutoReviewBudget>,
    scope_bytes: Option<usize>,
    interruption: Arc<Mutex<Option<ReviewInterruption>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReviewInterruptionStatus {
    Cancelled,
    Superseded,
}

#[derive(Clone, Debug)]
struct ReviewInterruption {
    status: ReviewInterruptionStatus,
    error_summary: String,
    superseded_by: Option<String>,
    cancel_reason: Option<String>,
}

#[derive(Debug)]
pub(crate) struct SavedReviewInterruption {
    pub(crate) status: ReviewInterruptionStatus,
    pub(crate) error_summary: String,
}

impl ReviewPersistenceContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn new(
        run_id: String,
        mode: ReviewPersistence,
        review_target: ReviewTarget,
        codex_home: &Path,
        cwd: &Path,
        model: Option<String>,
        reasoning_effort: Option<String>,
        prompt_token_estimate: Option<u64>,
    ) -> Self {
        let target = collect_auto_review_target(codex_home, cwd, &review_target).await;
        let store_scope = target
            .worktree_path
            .clone()
            .unwrap_or_else(|| cwd.to_path_buf());
        Self {
            run_id,
            source: match mode {
                ReviewPersistence::ManualAutoReview => AutoReviewRunSource::Manual,
                ReviewPersistence::BackgroundAutoReview => AutoReviewRunSource::Background,
            },
            store_scope,
            target,
            review_target,
            started_at_unix_secs: now_unix_secs(),
            model,
            reasoning_effort,
            prompt_token_estimate,
            background_budget: None,
            scope_bytes: None,
            interruption: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn is_manual(&self) -> bool {
        self.source == AutoReviewRunSource::Manual
    }

    pub(crate) fn is_background(&self) -> bool {
        self.source == AutoReviewRunSource::Background
    }

    pub(crate) fn run_id(&self) -> &str {
        &self.run_id
    }

    pub(crate) fn store_scope(&self) -> &Path {
        &self.store_scope
    }

    pub(crate) fn with_prompt_token_estimate(mut self, prompt_token_estimate: Option<u64>) -> Self {
        if self.prompt_token_estimate.is_none() {
            self.prompt_token_estimate = prompt_token_estimate;
        }
        self
    }

    pub(crate) fn with_background_budget(
        mut self,
        budget: AutoReviewBudget,
        scope_bytes: usize,
    ) -> Self {
        self.background_budget = Some(budget);
        self.scope_bytes = Some(scope_bytes);
        self
    }

    pub(crate) fn with_scope_bytes(mut self, scope_bytes: usize) -> Self {
        self.scope_bytes = Some(scope_bytes);
        self
    }

    pub(crate) fn background_budget(&self) -> Option<&AutoReviewBudget> {
        self.background_budget.as_ref()
    }

    pub(crate) fn target(&self) -> &AutoReviewRunTarget {
        &self.target
    }

    pub(crate) fn with_snapshot_epoch(mut self, snapshot_epoch: u64) -> Self {
        self.target.snapshot_epoch = Some(snapshot_epoch);
        self
    }

    pub(crate) fn worktree_diff_fingerprint(&self) -> Option<&str> {
        self.target.worktree_diff_fingerprint.as_deref()
    }

    pub(crate) fn dedupe_diff_fingerprint(&self) -> Option<&str> {
        self.worktree_diff_fingerprint()
            .or(match &self.review_target {
                ReviewTarget::CurrentTurnDiff { fingerprint } => Some(fingerprint.as_str()),
                _ => None,
            })
    }

    pub(crate) fn review_target(&self) -> &ReviewTarget {
        &self.review_target
    }

    pub(crate) async fn completion_freshness(&self, codex_home: &Path) -> AutoReviewRunFreshness {
        let active =
            collect_auto_review_target(codex_home, &self.store_scope, &self.review_target).await;
        match self.target.freshness(&active) {
            codex_auto_review::AutoReviewFreshness::Current => AutoReviewRunFreshness::Current,
            codex_auto_review::AutoReviewFreshness::Stale
            | codex_auto_review::AutoReviewFreshness::Detached => AutoReviewRunFreshness::Obsolete,
        }
    }

    pub(crate) fn save_running(&self, codex_home: impl AsRef<Path>) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Running,
            /*output*/ None,
            /*token_usage*/ None,
            /*error_summary*/ None,
        )
    }

    pub(crate) fn save_pending(&self, codex_home: impl AsRef<Path>) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Pending,
            /*output*/ None,
            /*token_usage*/ None,
            /*error_summary*/ None,
        )
    }

    pub(crate) fn save_completed_with_freshness(
        &self,
        codex_home: impl AsRef<Path>,
        output: &ReviewOutputEvent,
        token_usage: Option<&TokenUsage>,
        freshness: AutoReviewRunFreshness,
        usage: AutoReviewUsage,
    ) -> bool {
        let codex_home = codex_home.as_ref();
        let disposition = if output.findings.is_empty() {
            None
        } else if freshness == AutoReviewRunFreshness::Current {
            Some(AutoReviewFindingDispositionRecord {
                disposition: AutoReviewFindingDisposition::NeedsAttention,
                actor: AutoReviewDispositionActor::System,
                reason: None,
                updated_at_unix_secs: now_unix_secs(),
            })
        } else {
            Some(AutoReviewFindingDispositionRecord {
                disposition: AutoReviewFindingDisposition::Obsolete,
                actor: AutoReviewDispositionActor::System,
                reason: Some("review target changed before completion".to_string()),
                updated_at_unix_secs: now_unix_secs(),
            })
        };
        let has_stale_findings =
            freshness != AutoReviewRunFreshness::Current && !output.findings.is_empty();
        self.save_run_with_metadata_and_state(
            codex_home,
            AutoReviewRunStatus::Completed,
            Some(output),
            token_usage,
            /*error_summary*/ None,
            freshness,
            None,
            None,
            /*saved_token_estimate*/ None,
            move |state| {
                merge_usage(&mut state.usage, usage);
                state.finding_disposition = disposition;
                if has_stale_findings {
                    state.terminal_reason = Some(AutoReviewTerminalReason::StaleTarget);
                }
            },
        )
    }

    pub(crate) fn save_cancelled_with_summary(
        &self,
        codex_home: impl AsRef<Path>,
        error_summary: String,
    ) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Cancelled,
            /*output*/ None,
            /*token_usage*/ None,
            Some(error_summary),
        )
    }

    pub(crate) fn save_cancelled_with_summary_and_reason(
        &self,
        codex_home: impl AsRef<Path>,
        error_summary: String,
        cancel_reason: String,
    ) -> bool {
        self.save_run_with_metadata(
            codex_home,
            AutoReviewRunStatus::Cancelled,
            /*output*/ None,
            /*token_usage*/ None,
            Some(error_summary),
            AutoReviewRunFreshness::Current,
            None,
            Some(cancel_reason),
            /*saved_token_estimate*/ None,
        )
    }

    pub(crate) fn set_cancelled_interruption(&self, error_summary: String, cancel_reason: String) {
        self.set_interruption(ReviewInterruption {
            status: ReviewInterruptionStatus::Cancelled,
            error_summary,
            superseded_by: None,
            cancel_reason: Some(cancel_reason),
        });
    }

    pub(crate) fn set_superseded_interruption(
        &self,
        error_summary: String,
        superseded_by: Option<String>,
    ) {
        self.set_interruption(ReviewInterruption {
            status: ReviewInterruptionStatus::Superseded,
            error_summary,
            superseded_by,
            cancel_reason: None,
        });
    }

    pub(crate) fn save_interrupted(
        &self,
        codex_home: impl AsRef<Path>,
    ) -> Option<SavedReviewInterruption> {
        let interruption = {
            let pending = self.interruption.lock().unwrap_or_else(|err| {
                tracing::warn!("review interruption lock was poisoned; continuing");
                err.into_inner()
            });
            pending.clone()
        }
        .unwrap_or_else(|| ReviewInterruption {
            status: ReviewInterruptionStatus::Cancelled,
            error_summary: AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY.to_string(),
            superseded_by: None,
            cancel_reason: None,
        });
        let saved = match interruption.status {
            ReviewInterruptionStatus::Cancelled => {
                if let Some(cancel_reason) = interruption.cancel_reason.clone() {
                    self.save_cancelled_with_summary_and_reason(
                        codex_home,
                        interruption.error_summary.clone(),
                        cancel_reason,
                    )
                } else {
                    self.save_cancelled_with_summary(codex_home, interruption.error_summary.clone())
                }
            }
            ReviewInterruptionStatus::Superseded => self.save_superseded_with_summary(
                codex_home,
                interruption.error_summary.clone(),
                interruption.superseded_by,
            ),
        };
        saved.then_some(SavedReviewInterruption {
            status: interruption.status,
            error_summary: interruption.error_summary,
        })
    }

    pub(crate) fn save_superseded_with_summary(
        &self,
        codex_home: impl AsRef<Path>,
        error_summary: String,
        superseded_by: Option<String>,
    ) -> bool {
        self.save_run_with_metadata(
            codex_home,
            AutoReviewRunStatus::Superseded,
            /*output*/ None,
            /*token_usage*/ None,
            Some(error_summary),
            AutoReviewRunFreshness::Superseded,
            superseded_by,
            None,
            /*saved_token_estimate*/ None,
        )
    }

    pub(crate) fn save_failed(
        &self,
        codex_home: impl AsRef<Path>,
        error_summary: String,
        token_usage: Option<&TokenUsage>,
    ) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Failed,
            /*output*/ None,
            token_usage,
            Some(error_summary),
        )
    }

    pub(crate) fn save_empty_output(
        &self,
        codex_home: impl AsRef<Path>,
        error_summary: String,
        token_usage: Option<&TokenUsage>,
        usage: AutoReviewUsage,
    ) -> bool {
        let codex_home = codex_home.as_ref();
        self.save_run_with_metadata_and_state(
            codex_home,
            AutoReviewRunStatus::Failed,
            /*output*/ None,
            token_usage,
            Some(error_summary),
            AutoReviewRunFreshness::Current,
            None,
            Some(
                AutoReviewTerminalReason::EmptyOutput
                    .cancel_reason()
                    .to_string(),
            ),
            /*saved_token_estimate*/ None,
            move |state| {
                merge_usage(&mut state.usage, usage);
                state.terminal_reason = Some(AutoReviewTerminalReason::EmptyOutput);
            },
        )
    }

    pub(crate) fn save_budget_cancelled(
        &self,
        codex_home: impl AsRef<Path>,
        reason: AutoReviewTerminalReason,
        error_summary: String,
        token_usage: Option<&TokenUsage>,
        usage: AutoReviewUsage,
    ) -> bool {
        let codex_home = codex_home.as_ref();
        self.save_run_with_metadata_and_state(
            codex_home,
            AutoReviewRunStatus::Cancelled,
            /*output*/ None,
            token_usage,
            Some(error_summary),
            AutoReviewRunFreshness::Current,
            None,
            Some(reason.cancel_reason().to_string()),
            /*saved_token_estimate*/ None,
            move |state| {
                merge_usage(&mut state.usage, usage);
                state.terminal_reason = Some(reason);
            },
        )
    }

    pub(crate) fn save_budget_skipped(
        &self,
        codex_home: impl AsRef<Path>,
        reason: AutoReviewTerminalReason,
        error_summary: String,
        usage: AutoReviewUsage,
    ) -> bool {
        let codex_home = codex_home.as_ref();
        self.save_run_with_metadata_and_state(
            codex_home,
            AutoReviewRunStatus::Skipped,
            /*output*/ None,
            /*token_usage*/ None,
            Some(error_summary),
            AutoReviewRunFreshness::Current,
            None,
            Some(reason.cancel_reason().to_string()),
            /*saved_token_estimate*/ None,
            move |state| {
                merge_usage(&mut state.usage, usage);
                state.terminal_reason = Some(reason);
            },
        )
    }

    pub(crate) fn record_progress(
        &self,
        codex_home: impl AsRef<Path>,
        usage: AutoReviewUsage,
    ) -> bool {
        self.update_run_state(codex_home.as_ref(), |state| {
            merge_usage(&mut state.usage, usage);
        })
    }

    pub(crate) fn save_skipped(&self, codex_home: impl AsRef<Path>, error_summary: String) -> bool {
        self.save_run(
            codex_home,
            AutoReviewRunStatus::Skipped,
            /*output*/ None,
            /*token_usage*/ None,
            Some(error_summary),
        )
    }

    pub(crate) fn save_skipped_duplicate(
        &self,
        codex_home: impl AsRef<Path>,
        duplicate: &AutoReviewDuplicateMatch,
    ) -> bool {
        let error_summary = format!(
            "equivalent background auto review already exists: {}",
            duplicate.run_id
        );
        self.save_run_with_metadata(
            codex_home,
            AutoReviewRunStatus::Skipped,
            /*output*/ None,
            /*token_usage*/ None,
            Some(error_summary),
            AutoReviewRunFreshness::Superseded,
            Some(duplicate.run_id.clone()),
            Some("duplicate_auto_review_scope".to_string()),
            duplicate_saved_token_estimate(duplicate),
        )
    }

    fn save_run(
        &self,
        codex_home: impl AsRef<Path>,
        status: AutoReviewRunStatus,
        output: Option<&ReviewOutputEvent>,
        token_usage: Option<&TokenUsage>,
        error_summary: Option<String>,
    ) -> bool {
        self.save_run_with_metadata(
            codex_home,
            status,
            output,
            token_usage,
            error_summary,
            AutoReviewRunFreshness::Current,
            None,
            None,
            /*saved_token_estimate*/ None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn save_run_with_metadata(
        &self,
        codex_home: impl AsRef<Path>,
        status: AutoReviewRunStatus,
        output: Option<&ReviewOutputEvent>,
        token_usage: Option<&TokenUsage>,
        error_summary: Option<String>,
        freshness: AutoReviewRunFreshness,
        superseded_by: Option<String>,
        cancel_reason: Option<String>,
        saved_token_estimate: Option<u64>,
    ) -> bool {
        self.save_run_with_metadata_and_state(
            codex_home,
            status,
            output,
            token_usage,
            error_summary,
            freshness,
            superseded_by,
            cancel_reason,
            saved_token_estimate,
            |_| {},
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn save_run_with_metadata_and_state(
        &self,
        codex_home: impl AsRef<Path>,
        status: AutoReviewRunStatus,
        output: Option<&ReviewOutputEvent>,
        token_usage: Option<&TokenUsage>,
        error_summary: Option<String>,
        freshness: AutoReviewRunFreshness,
        superseded_by: Option<String>,
        cancel_reason: Option<String>,
        saved_token_estimate: Option<u64>,
        update_state: impl FnOnce(&mut codex_auto_review::AutoReviewRunState),
    ) -> bool {
        let codex_home = codex_home.as_ref();
        let completed_at_unix_secs = if is_terminal_status(&status) {
            Some(now_unix_secs())
        } else {
            None
        };
        let store = AutoReviewStore::for_scope(codex_home, &self.store_scope);
        let _guard = AUTO_REVIEW_RUN_WRITE_LOCK.lock().unwrap_or_else(|err| {
            tracing::warn!("auto review run write lock was poisoned; continuing");
            err.into_inner()
        });
        if let Ok(existing) = store.load_run(&self.run_id)
            && is_terminal_status(&existing.status)
        {
            let corrects_interrupted_lifecycle = existing.status == AutoReviewRunStatus::Cancelled
                && existing.cancel_reason.is_none()
                && existing.superseded_by.is_none()
                && is_explicit_lifecycle_update(
                    &status,
                    superseded_by.as_deref(),
                    cancel_reason.as_deref(),
                );
            if !corrects_interrupted_lifecycle {
                if !is_terminal_status(&status) {
                    tracing::debug!(
                        run_id = %self.run_id,
                        existing_status = ?existing.status,
                        "skipping non-terminal auto review run write after terminal status"
                    );
                }
                return false;
            }
        }
        if let Err(err) = store.list_runs() {
            tracing::warn!(
                run_id = %self.run_id,
                error = %err,
                "failed to read auto review runs before persisting output"
            );
            return false;
        }
        if !self.update_run_state(codex_home, update_state) {
            return false;
        }
        if let Some(output) = output
            && let Err(err) = store.save_output(&self.run_id, output)
        {
            tracing::warn!(
                run_id = %self.run_id,
                error = %err,
                "failed to persist auto review output"
            );
            return false;
        }
        let finding_count = output
            .map(|output| output.findings.len())
            .unwrap_or_default();
        let finding_digests = output.map(finding_digests).unwrap_or_default();
        let omitted_finding_digest_count = finding_count.saturating_sub(finding_digests.len());
        let token_count = token_usage.and_then(token_count_u64);
        let run = AutoReviewRun {
            schema_version: SCHEMA_VERSION,
            run_id: self.run_id.clone(),
            status,
            freshness,
            source: self.source.clone(),
            target: self.target.clone(),
            review_target: self.review_target.clone(),
            started_at_unix_secs: self.started_at_unix_secs,
            completed_at_unix_secs,
            model: self.model.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            prompt_token_estimate: self.prompt_token_estimate,
            token_count,
            saved_token_estimate,
            superseded_by,
            cancel_reason,
            error_summary,
            finding_count,
            finding_digests,
            omitted_finding_digest_count,
        };
        if let Err(err) = store.save_run(&run) {
            tracing::warn!(
                run_id = %self.run_id,
                error = %err,
                "failed to persist auto review run"
            );
            return false;
        }
        true
    }

    fn update_run_state(
        &self,
        codex_home: &Path,
        update: impl FnOnce(&mut codex_auto_review::AutoReviewRunState),
    ) -> bool {
        let store = AutoReviewStore::for_scope(codex_home, &self.store_scope);
        match store.update_run_state(&self.run_id, |state| {
            if let Some(budget) = &self.background_budget {
                state.budget = Some(budget.clone());
            }
            if let Some(scope_bytes) = self.scope_bytes {
                state.usage.scope_bytes = Some(scope_bytes);
            }
            update(state);
            Ok(())
        }) {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(
                    run_id = %self.run_id,
                    error = %err,
                    "failed to persist auto review run state"
                );
                false
            }
        }
    }

    fn set_interruption(&self, interruption: ReviewInterruption) {
        let mut pending = self.interruption.lock().unwrap_or_else(|err| {
            tracing::warn!("review interruption lock was poisoned; continuing");
            err.into_inner()
        });
        *pending = Some(interruption);
    }
}

fn is_terminal_status(status: &AutoReviewRunStatus) -> bool {
    status.is_terminal()
}

fn is_explicit_lifecycle_update(
    status: &AutoReviewRunStatus,
    superseded_by: Option<&str>,
    cancel_reason: Option<&str>,
) -> bool {
    matches!(
        status,
        AutoReviewRunStatus::Cancelled | AutoReviewRunStatus::Superseded
    ) && (status == &AutoReviewRunStatus::Superseded
        || superseded_by.is_some()
        || cancel_reason.is_some())
}

fn token_count_u64(token_usage: &TokenUsage) -> Option<u64> {
    let reconstructed_total = token_usage
        .non_cached_input()
        .saturating_add(token_usage.cached_input())
        .saturating_add(token_usage.output_tokens.max(0));
    u64::try_from(token_usage.total_tokens.max(reconstructed_total))
        .ok()
        .filter(|token_count| *token_count > 0)
}

fn merge_usage(current: &mut AutoReviewUsage, update: AutoReviewUsage) {
    if update.scope_bytes.is_some() {
        current.scope_bytes = update.scope_bytes;
    }
    if update.elapsed_ms.is_some() {
        current.elapsed_ms = update.elapsed_ms;
    }
    if update.total_tokens.is_some() {
        current.total_tokens = update.total_tokens;
    }
    if update.output_bytes.is_some() {
        current.output_bytes = update.output_bytes;
    }
    if update.finding_count.is_some() {
        current.finding_count = update.finding_count;
    }
}

fn duplicate_saved_token_estimate(duplicate: &AutoReviewDuplicateMatch) -> Option<u64> {
    duplicate.token_count.or(duplicate.prompt_token_estimate)
}

pub(crate) async fn collect_auto_review_target(
    codex_home: &Path,
    cwd: &Path,
    review_target: &ReviewTarget,
) -> AutoReviewRunTarget {
    let git_info = collect_git_info(cwd).await;
    let repo_root = get_git_repo_root(cwd);
    let store_scope = repo_root.as_deref().unwrap_or(cwd);
    let snapshot_epoch =
        match ReviewCoordination::for_scope(codex_home, store_scope).current_snapshot_epoch() {
            Ok(0) => None,
            Ok(epoch) => Some(epoch),
            Err(err) => {
                tracing::warn!(error = %err, "failed to collect auto review snapshot epoch");
                None
            }
        };
    let base_sha = match (repo_root.as_deref(), review_target) {
        (Some(repo_root), ReviewTarget::BaseBranch { branch }) => {
            match merge_base_with_head(repo_root, branch) {
                Ok(base_sha) => base_sha,
                Err(err) => {
                    tracing::warn!(
                        branch,
                        error = %err,
                        "failed to collect auto review base sha"
                    );
                    None
                }
            }
        }
        _ => None,
    };

    let worktree_diff_fingerprint = match review_target {
        ReviewTarget::UncommittedChanges | ReviewTarget::CurrentTurnDiff { .. } => {
            get_worktree_diff_fingerprint(cwd).await
        }
        _ => None,
    };

    AutoReviewRunTarget {
        branch: git_info.as_ref().and_then(|git| git.branch.clone()),
        head_sha: git_info
            .as_ref()
            .and_then(|git| git.commit_hash.as_ref().map(|sha| sha.0.clone())),
        base_sha,
        worktree_path: repo_root.or_else(|| Some(PathBuf::from(cwd))),
        snapshot_epoch,
        snapshot_commit: git_info
            .as_ref()
            .and_then(|git| git.commit_hash.as_ref().map(|sha| sha.0.clone())),
        head_at_launch: git_info
            .as_ref()
            .and_then(|git| git.commit_hash.as_ref().map(|sha| sha.0.clone())),
        worktree_diff_fingerprint,
    }
}

fn now_unix_secs() -> i64 {
    now_unix_timestamp_ms() / 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::ReviewCodeLocation;
    use codex_protocol::protocol::ReviewFinding;
    use codex_protocol::protocol::ReviewLineRange;
    use codex_protocol::protocol::ReviewPersistence;
    use codex_protocol::protocol::TokenUsage;
    use tempfile::TempDir;

    #[tokio::test]
    async fn save_running_does_not_overwrite_terminal_run() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "late-running".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await;

        persistence.save_cancelled_with_summary(
            codex_home.path(),
            AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY.to_string(),
        );
        persistence.save_running(codex_home.path());

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("late-running")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
    }

    #[tokio::test]
    async fn save_cancelled_with_summary_records_custom_reason() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "custom-cancelled".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await;

        persistence.save_cancelled_with_summary(
            codex_home.path(),
            "background auto review was cancelled by request".to_string(),
        );

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("custom-cancelled")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
        assert_eq!(
            run.error_summary.as_deref(),
            Some("background auto review was cancelled by request")
        );
    }

    #[tokio::test]
    async fn explicit_cancel_reason_corrects_generic_interruption() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "corrected-cancelled".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await;

        persistence.save_cancelled_with_summary(
            codex_home.path(),
            AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY.to_string(),
        );
        persistence.save_cancelled_with_summary_and_reason(
            codex_home.path(),
            "background auto review was cancelled by request".to_string(),
            "background_auto_review_control".to_string(),
        );

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("corrected-cancelled")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
        assert_eq!(
            run.error_summary.as_deref(),
            Some("background auto review was cancelled by request")
        );
        assert_eq!(
            run.cancel_reason.as_deref(),
            Some("background_auto_review_control")
        );
    }

    #[tokio::test]
    async fn explicit_supersede_corrects_generic_interruption() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "corrected-superseded".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await;

        persistence.save_cancelled_with_summary(
            codex_home.path(),
            AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY.to_string(),
        );
        persistence.save_superseded_with_summary(
            codex_home.path(),
            "background auto review was superseded by run replacement-run".to_string(),
            Some("replacement-run".to_string()),
        );

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("corrected-superseded")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Superseded);
        assert_eq!(run.freshness, AutoReviewRunFreshness::Superseded);
        assert_eq!(run.superseded_by.as_deref(), Some("replacement-run"));
    }

    #[tokio::test]
    async fn save_superseded_with_summary_records_supersede_metadata() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "custom-superseded".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await;

        persistence.save_superseded_with_summary(
            codex_home.path(),
            "background auto review was superseded by run replacement-run".to_string(),
            Some("replacement-run".to_string()),
        );
        persistence.save_running(codex_home.path());

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("custom-superseded")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Superseded);
        assert_eq!(run.freshness, AutoReviewRunFreshness::Superseded);
        assert_eq!(run.superseded_by.as_deref(), Some("replacement-run"));
        assert!(run.completed_at_unix_secs.is_some());
        assert_eq!(
            run.error_summary.as_deref(),
            Some("background auto review was superseded by run replacement-run")
        );
    }

    #[tokio::test]
    async fn save_pending_can_advance_to_running() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "pending-running".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await;

        persistence.save_pending(codex_home.path());
        persistence.save_running(codex_home.path());

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("pending-running")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Running);
        assert_eq!(run.completed_at_unix_secs, None);
    }

    #[tokio::test]
    async fn save_skipped_blocks_late_running() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "skipped-running".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await;

        persistence.save_skipped(codex_home.path(), "duplicate fingerprint".to_string());
        persistence.save_running(codex_home.path());

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("skipped-running")
            .expect("load persisted review run");
        assert_eq!(run.status, AutoReviewRunStatus::Skipped);
        assert_eq!(run.error_summary.as_deref(), Some("duplicate fingerprint"));
    }

    #[tokio::test]
    async fn save_skipped_duplicate_records_duplicate_metadata() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "duplicate-candidate".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await;
        let duplicate = AutoReviewDuplicateMatch {
            run_id: "existing-run".to_string(),
            status: AutoReviewRunStatus::Completed,
            disposition: codex_auto_review::AutoReviewDuplicateDisposition::ReuseTerminal,
            finding_count: 1,
            model: Some("test-model".to_string()),
            token_count: Some(25_915),
            prompt_token_estimate: Some(42_000),
        };

        persistence.save_skipped_duplicate(codex_home.path(), &duplicate);

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("duplicate-candidate")
            .expect("load persisted duplicate skip");
        assert_eq!(run.status, AutoReviewRunStatus::Skipped);
        assert_eq!(run.freshness, AutoReviewRunFreshness::Superseded);
        assert_eq!(run.superseded_by.as_deref(), Some("existing-run"));
        assert_eq!(
            run.cancel_reason.as_deref(),
            Some("duplicate_auto_review_scope")
        );
        assert_eq!(
            run.error_summary.as_deref(),
            Some("equivalent background auto review already exists: existing-run")
        );
        assert_eq!(run.saved_token_estimate, Some(25_915));
    }

    #[tokio::test]
    async fn save_completed_records_model_reasoning_and_token_diagnostics() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "completed-metadata".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            Some("medium".to_string()),
            Some(42_000),
        )
        .await;
        let output = ReviewOutputEvent::default();
        let token_usage = TokenUsage {
            input_tokens: 100,
            cached_input_tokens: 90,
            cache_write_input_tokens: 0,
            output_tokens: 10,
            reasoning_output_tokens: 0,
            total_tokens: 20,
        };

        persistence.save_completed_with_freshness(
            codex_home.path(),
            &output,
            Some(&token_usage),
            AutoReviewRunFreshness::Current,
            AutoReviewUsage {
                total_tokens: Some(110),
                finding_count: Some(0),
                ..Default::default()
            },
        );

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("completed-metadata")
            .expect("load persisted review run");
        assert_eq!(run.model.as_deref(), Some("test-model"));
        assert_eq!(run.reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(run.prompt_token_estimate, Some(42_000));
        assert_eq!(run.token_count, Some(110));
        assert_eq!(run.saved_token_estimate, None);
    }

    #[tokio::test]
    async fn save_budget_cancelled_records_limits_usage_and_reason() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "budget-cancelled".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await
        .with_background_budget(test_budget(), 12_000);
        let usage = AutoReviewUsage {
            scope_bytes: Some(12_000),
            elapsed_ms: Some(30_000),
            total_tokens: Some(250_001),
            ..Default::default()
        };
        let token_usage = TokenUsage {
            total_tokens: 250_001,
            ..Default::default()
        };

        assert!(persistence.save_budget_cancelled(
            codex_home.path(),
            AutoReviewTerminalReason::BudgetTotalTokens,
            "background review exceeded token budget".to_string(),
            Some(&token_usage),
            usage.clone(),
        ));

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("budget-cancelled")
            .expect("load persisted review run");
        let state = store
            .load_run_state("budget-cancelled")
            .expect("load persisted review state")
            .expect("review state should exist");
        assert_eq!(run.status, AutoReviewRunStatus::Cancelled);
        assert_eq!(run.cancel_reason.as_deref(), Some("budget_total_tokens"));
        assert_eq!(run.token_count, Some(250_001));
        assert_eq!(state.budget, Some(test_budget()));
        assert_eq!(state.usage, usage);
        assert_eq!(
            state.terminal_reason,
            Some(AutoReviewTerminalReason::BudgetTotalTokens)
        );
    }

    #[tokio::test]
    async fn completed_current_findings_require_attention() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "actionable-findings".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await
        .with_background_budget(test_budget(), 12_000);
        let output = output_with_finding(cwd.path());

        assert!(persistence.save_completed_with_freshness(
            codex_home.path(),
            &output,
            /*token_usage*/ None,
            AutoReviewRunFreshness::Current,
            AutoReviewUsage {
                finding_count: Some(1),
                ..Default::default()
            },
        ));

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let state = store
            .load_run_state("actionable-findings")
            .expect("load persisted review state")
            .expect("review state should exist");
        let disposition = state
            .finding_disposition
            .expect("current findings should require disposition");
        assert_eq!(
            disposition.disposition,
            AutoReviewFindingDisposition::NeedsAttention
        );
        assert_eq!(disposition.actor, AutoReviewDispositionActor::System);
    }

    #[tokio::test]
    async fn completed_stale_findings_are_marked_obsolete() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        let persistence = ReviewPersistenceContext::new(
            "stale-findings".to_string(),
            ReviewPersistence::BackgroundAutoReview,
            ReviewTarget::UncommittedChanges,
            codex_home.path(),
            cwd.path(),
            Some("test-model".to_string()),
            /*reasoning_effort*/ None,
            /*prompt_token_estimate*/ None,
        )
        .await
        .with_background_budget(test_budget(), 12_000);
        let output = output_with_finding(cwd.path());

        assert!(persistence.save_completed_with_freshness(
            codex_home.path(),
            &output,
            /*token_usage*/ None,
            AutoReviewRunFreshness::Obsolete,
            AutoReviewUsage {
                finding_count: Some(1),
                ..Default::default()
            },
        ));

        let store = AutoReviewStore::for_scope(codex_home.path(), cwd.path());
        let run = store
            .load_run("stale-findings")
            .expect("load persisted review run");
        let state = store
            .load_run_state("stale-findings")
            .expect("load persisted review state")
            .expect("review state should exist");
        assert_eq!(run.freshness, AutoReviewRunFreshness::Obsolete);
        assert_eq!(
            state.terminal_reason,
            Some(AutoReviewTerminalReason::StaleTarget)
        );
        assert_eq!(
            state
                .finding_disposition
                .expect("stale finding disposition")
                .disposition,
            AutoReviewFindingDisposition::Obsolete
        );
    }

    fn test_budget() -> AutoReviewBudget {
        AutoReviewBudget {
            max_scope_bytes: 120_000,
            max_elapsed_ms: 300_000,
            max_total_tokens: 250_000,
            max_output_bytes: 65_536,
            max_findings: 20,
        }
    }

    fn output_with_finding(cwd: &Path) -> ReviewOutputEvent {
        ReviewOutputEvent {
            findings: vec![ReviewFinding {
                title: "[P1] Finding".to_string(),
                body: "Finding body".to_string(),
                confidence_score: 0.9,
                priority: 1,
                code_location: ReviewCodeLocation {
                    absolute_file_path: cwd.join("src/lib.rs"),
                    line_range: ReviewLineRange { start: 1, end: 2 },
                },
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn collect_target_records_current_snapshot_epoch() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");

        let target = collect_auto_review_target(
            codex_home.path(),
            cwd.path(),
            &ReviewTarget::UncommittedChanges,
        )
        .await;
        assert_eq!(target.snapshot_epoch, None);

        ReviewCoordination::for_scope(codex_home.path(), cwd.path())
            .bump_snapshot_epoch()
            .expect("bump snapshot epoch");

        let target = collect_auto_review_target(
            codex_home.path(),
            cwd.path(),
            &ReviewTarget::UncommittedChanges,
        )
        .await;

        assert_eq!(target.snapshot_epoch, Some(1));
    }

    #[tokio::test]
    async fn current_turn_diff_target_records_live_worktree_fingerprint() {
        let codex_home = TempDir::new().expect("create temp codex home");
        let cwd = TempDir::new().expect("create temp cwd");
        std::process::Command::new("git")
            .arg("init")
            .current_dir(cwd.path())
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(cwd.path())
            .output()
            .expect("git config user.email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(cwd.path())
            .output()
            .expect("git config user.name");
        std::fs::write(cwd.path().join("tracked.txt"), "base\n").expect("write base file");
        std::process::Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(cwd.path())
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(cwd.path())
            .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00Z")
            .output()
            .expect("git commit");
        std::fs::write(cwd.path().join("tracked.txt"), "base\nchange\n").expect("write dirty file");
        let expected = get_worktree_diff_fingerprint(cwd.path())
            .await
            .expect("dirty worktree fingerprint");

        let target = collect_auto_review_target(
            codex_home.path(),
            cwd.path(),
            &ReviewTarget::CurrentTurnDiff {
                fingerprint: "sha256:synthetic-turn-diff".to_string(),
            },
        )
        .await;

        assert_eq!(
            target.worktree_diff_fingerprint.as_deref(),
            Some(expected.as_str())
        );
    }
}
