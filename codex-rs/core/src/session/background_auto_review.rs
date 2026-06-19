use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use codex_auto_review::AutoReviewDuplicateDisposition;
use codex_auto_review::AutoReviewDuplicateMatch;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::ReviewCoordination;
use codex_auto_review::ReviewLockGuard;
use codex_git_utils::diff_fingerprint;
use codex_git_utils::get_git_repo_root;
use codex_git_utils::get_worktree_diff_fingerprint;
use codex_protocol::protocol::BackgroundAutoReviewControlAction;
use codex_protocol::protocol::BackgroundAutoReviewControlReason;
use codex_protocol::protocol::BackgroundAutoReviewStatus;
use codex_protocol::protocol::ReviewPersistence;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_utils_absolute_path::AbsolutePathBuf;
use tracing::debug;
use tracing::warn;

use super::review::ReviewPersistenceSpec;
use super::review::prepare_review_thread;
use super::review::record_background_review_status;
use super::review::spawn_detached_review_thread;
use super::session::Session;
use super::turn_context::TurnContext;
use crate::review_persistence::AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY;
use crate::review_persistence::ReviewPersistenceContext;
use crate::review_prompts::resolve_review_request;
use crate::state::BackgroundAutoReviewControlledRun;
use crate::state::BackgroundAutoReviewRunningHandle;
use crate::turn_timing::now_unix_timestamp_ms;

const BACKGROUND_AUTO_REVIEW_DEBOUNCE: Duration = Duration::from_secs(2);
const BACKGROUND_AUTO_REVIEW_LOCK_RETRY: Duration = Duration::from_millis(500);
const BACKGROUND_AUTO_REVIEW_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);

impl Session {
    pub(crate) async fn record_background_auto_review_turn_start(
        self: &Arc<Self>,
        turn_context: &TurnContext,
    ) {
        {
            let mut state = self.state.lock().await;
            state
                .background_auto_review
                .begin_regular_turn_pending_fingerprint(turn_context.sub_id.clone());
        }
        let fingerprint = background_review_fingerprint(turn_context).await;
        let mut state = self.state.lock().await;
        state
            .background_auto_review
            .record_regular_turn_start_fingerprint(&turn_context.sub_id, fingerprint);
    }

    pub(crate) async fn maybe_schedule_background_auto_review(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        turn_diff: Option<String>,
    ) {
        let sess = Arc::clone(self);
        tokio::spawn(async move {
            sess.schedule_background_auto_review_after_turn(turn_context, turn_diff)
                .await;
        });
    }

    async fn schedule_background_auto_review_after_turn(
        self: Arc<Self>,
        turn_context: Arc<TurnContext>,
        turn_diff: Option<String>,
    ) {
        let start = {
            let mut state = self.state.lock().await;
            state
                .background_auto_review
                .take_regular_turn_start(&turn_context.sub_id)
        };
        let Some(start) = start else {
            return;
        };
        let Some(after_fingerprint) = background_review_fingerprint(turn_context.as_ref()).await
        else {
            let mut state = self.state.lock().await;
            let _ = state
                .background_auto_review
                .complete_regular_turn_from_start(start, None);
            debug!(turn_id = %turn_context.sub_id, "background auto review skipped: clean or unsupported worktree");
            return;
        };
        let schedule = {
            let mut state = self.state.lock().await;
            state
                .background_auto_review
                .complete_regular_turn_from_start(start, Some(after_fingerprint))
        };
        let Some(schedule) = schedule else {
            debug!(turn_id = %turn_context.sub_id, "background auto review skipped: unchanged or duplicate fingerprint");
            return;
        };
        let Some(cwd) = turn_context
            .environments
            .single_local_environment_cwd()
            .cloned()
        else {
            debug!("background auto review skipped: no single local worktree");
            return;
        };
        let Some(turn_diff) = turn_diff.and_then(non_empty_turn_diff) else {
            debug!("background auto review skipped: current-turn diff unavailable");
            return;
        };
        let Some(turn_diff_fingerprint) = diff_fingerprint(&turn_diff) else {
            debug!("background auto review skipped: current-turn diff unavailable");
            return;
        };
        let review_target = ReviewTarget::CurrentTurnDiff {
            fingerprint: turn_diff_fingerprint,
        };
        let model = turn_context
            .config
            .review_model
            .clone()
            .unwrap_or_else(|| turn_context.model_info.slug.clone());
        let codex_home = self.codex_home().await;
        let persistence = ReviewPersistenceContext::new(
            uuid::Uuid::new_v4().to_string(),
            ReviewPersistence::BackgroundAutoReview,
            review_target.clone(),
            codex_home.as_ref(),
            cwd.as_ref(),
            Some(model),
        )
        .await;
        if let Some(duplicate) = self
            .resolve_durable_background_auto_review_duplicate(&persistence)
            .await
        {
            self.record_skipped_duplicate_background_auto_review(
                &persistence,
                schedule.generation,
                &schedule.fingerprint,
                &duplicate,
            )
            .await;
            debug!(
                duplicate_run_id = %duplicate.run_id,
                duplicate_status = ?duplicate.status,
                "background auto review skipped: equivalent durable review already exists"
            );
            return;
        }
        if !self
            .record_pending_background_auto_review(
                schedule.generation,
                &schedule.fingerprint,
                persistence.clone(),
            )
            .await
        {
            self.record_skipped_background_auto_review(
                &persistence,
                schedule.generation,
                &schedule.fingerprint,
                "background auto review schedule was superseded before pending".to_string(),
            )
            .await;
            debug!("background auto review skipped before debounce: schedule superseded");
            return;
        }
        let codex_home = self.codex_home().await;
        if persistence.save_pending(codex_home) {
            record_background_review_status(
                Arc::clone(&self),
                &persistence,
                BackgroundAutoReviewStatus::Pending,
                None,
            )
            .await;
        }
        self.supersede_clean_durable_background_auto_review_duplicates(&persistence)
            .await;

        let sess = Arc::clone(&self);
        tokio::spawn(async move {
            tokio::time::sleep(BACKGROUND_AUTO_REVIEW_DEBOUNCE).await;
            if !sess
                .is_current_background_auto_review_schedule(
                    schedule.generation,
                    &schedule.fingerprint,
                )
                .await
            {
                sess.record_skipped_background_auto_review(
                    &persistence,
                    schedule.generation,
                    &schedule.fingerprint,
                    "background auto review schedule was superseded during debounce".to_string(),
                )
                .await;
                debug!("background auto review debounce superseded");
                return;
            }
            sess.start_detached_background_auto_review(
                turn_context,
                schedule.generation,
                schedule.fingerprint,
                persistence,
                turn_diff,
            )
            .await;
        });
    }

    async fn is_current_background_auto_review_schedule(
        &self,
        generation: u64,
        _fingerprint: &str,
    ) -> bool {
        let state = self.state.lock().await;
        state.background_auto_review.is_current_schedule(generation)
    }

    async fn start_detached_background_auto_review(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        generation: u64,
        fingerprint: String,
        persistence: ReviewPersistenceContext,
        turn_diff: String,
    ) {
        if self.input_queue.has_trigger_turn_mailbox_items().await
            || self.active_turn.lock().await.is_some()
        {
            self.record_skipped_background_auto_review(
                &persistence,
                generation,
                &fingerprint,
                "foreground work was pending or active before background auto review could start"
                    .to_string(),
            )
            .await;
            debug!("background auto review skipped: foreground work pending or active");
            return;
        }

        let Some(cwd) = turn_context.environments.single_local_environment_cwd() else {
            debug!("background auto review skipped: no single local worktree");
            return;
        };
        let sub_id = persistence.run_id().to_string();
        let review_request = ReviewRequest {
            target: ReviewTarget::Custom {
                instructions: background_auto_review_turn_diff_prompt(&turn_diff),
            },
            user_facing_hint: Some("current turn changes".to_string()),
        };
        let resolved = match resolve_review_request(review_request, cwd) {
            Ok(resolved) => resolved,
            Err(err) => {
                warn!(error = %err, "background auto review request resolution failed");
                return;
            }
        };
        if let Some(max_diff_bytes) = turn_context.config.background_auto_review_max_diff_bytes {
            let diff_byte_count = turn_diff.len();
            if diff_byte_count > max_diff_bytes {
                let error_summary = format!(
                    "diff exceeds background review size limit: {diff_byte_count} bytes > \
                     {max_diff_bytes} bytes"
                );
                debug!(%error_summary, "background auto review skipped: oversized diff");
                self.record_skipped_background_auto_review(
                    &persistence,
                    generation,
                    &fingerprint,
                    error_summary,
                )
                .await;
                return;
            }
        }

        if self.input_queue.has_trigger_turn_mailbox_items().await
            || self.active_turn.lock().await.is_some()
        {
            debug!(
                "background auto review skipped after prepare: foreground work pending or active"
            );
            let error_summary =
                "foreground work became active before background auto review could start"
                    .to_string();
            self.record_skipped_background_auto_review(
                &persistence,
                generation,
                &fingerprint,
                error_summary,
            )
            .await;
            return;
        }
        let coordination =
            ReviewCoordination::for_scope(self.codex_home().await, persistence.store_scope());
        let review_lock_guard = match acquire_background_auto_review_lock(
            &coordination,
            format!("background_auto_review:{}", persistence.run_id()),
        )
        .await
        {
            Ok(Some(guard)) => guard,
            Ok(None) => {
                let error_summary =
                    "another background auto review is already running for this worktree"
                        .to_string();
                self.record_skipped_background_auto_review(
                    &persistence,
                    generation,
                    &fingerprint,
                    error_summary,
                )
                .await;
                debug!("background auto review skipped: review lock is held");
                return;
            }
            Err(err) => {
                let error_summary = format!("failed to acquire background auto review lock: {err}");
                self.record_skipped_background_auto_review(
                    &persistence,
                    generation,
                    &fingerprint,
                    error_summary,
                )
                .await;
                warn!(error = %err, "failed to acquire background auto review lock");
                return;
            }
        };
        let prepared = prepare_review_thread(
            Arc::clone(self),
            Arc::clone(&turn_context.config),
            Arc::clone(&turn_context),
            sub_id,
            resolved,
            Some(ReviewPersistenceSpec::Context(persistence.clone())),
        )
        .await;
        let Some(persistence) = prepared.task.persistence_context() else {
            debug!("background auto review skipped after prepare: missing persistence context");
            return;
        };
        if self.input_queue.has_trigger_turn_mailbox_items().await
            || self.active_turn.lock().await.is_some()
        {
            debug!(
                "background auto review skipped after prepare: foreground work pending or active"
            );
            let error_summary =
                "foreground work became active before background auto review could start"
                    .to_string();
            self.record_skipped_background_auto_review(
                &persistence,
                generation,
                &fingerprint,
                error_summary,
            )
            .await;
            return;
        }
        let started_review = match self
            .record_started_background_auto_review_if_idle(
                generation,
                &fingerprint,
                persistence.clone(),
                &coordination,
            )
            .await
        {
            Ok(Some(started_review)) => started_review,
            Ok(None) => {
                let error_summary =
                    "background auto review schedule was superseded or foreground work became \
                     active before start"
                        .to_string();
                self.record_skipped_background_auto_review(
                    &persistence,
                    generation,
                    &fingerprint,
                    error_summary,
                )
                .await;
                debug!(
                    "background auto review skipped after prepare: schedule superseded or \
                     foreground work active"
                );
                return;
            }
            Err(err) => {
                let error_summary =
                    format!("failed to bump background auto review snapshot epoch: {err}");
                self.record_skipped_background_auto_review(
                    &persistence,
                    generation,
                    &fingerprint,
                    error_summary,
                )
                .await;
                warn!(error = %err, "failed to bump background auto review snapshot epoch");
                return;
            }
        };
        let persistence = started_review.running_review.persistence.clone();
        if let Some(displaced_running_review) = started_review.displaced_running_review {
            self.cancel_running_background_auto_review(
                displaced_running_review,
                AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY.to_string(),
            )
            .await;
        }
        if started_review
            .running_review
            .cancellation_token
            .is_cancelled()
        {
            debug!("background auto review skipped after lock: review was cancelled");
            return;
        }
        spawn_detached_review_thread(
            Arc::clone(self),
            prepared.with_persistence(persistence),
            started_review.running_review,
            review_lock_guard,
            generation,
        );
    }

    pub(crate) async fn recover_auto_review_after_restart(&self) {
        let (codex_home, scopes) = {
            let state = self.state.lock().await;
            if state
                .session_configuration
                .session_source
                .is_non_root_agent()
            {
                return;
            }
            let mut scopes = vec![state.session_configuration.cwd.clone()];
            scopes.extend(
                state
                    .session_configuration
                    .environments
                    .iter()
                    .map(|environment| environment.cwd.clone()),
            );
            (state.session_configuration.codex_home().clone(), scopes)
        };
        let mut seen_scopes = HashSet::new();
        for cwd in scopes {
            let scope =
                get_git_repo_root(cwd.as_ref()).unwrap_or_else(|| cwd.as_ref().to_path_buf());
            if !seen_scopes.insert(scope.clone()) {
                continue;
            }
            let coordination = ReviewCoordination::for_scope(&codex_home, &scope);
            if let Err(err) = coordination.clear_stale_lock_if_dead() {
                warn!(error = %err, "failed to clear stale background auto review lock");
                continue;
            }
            let live_run_id = match coordination.read_lock_info() {
                Ok(Some(lock_info)) => auto_review_run_id_from_lock_intent(&lock_info.intent),
                Ok(None) => None,
                Err(err) => {
                    warn!(error = %err, "failed to read background auto review lock");
                    continue;
                }
            };
            let store = AutoReviewStore::for_scope(&codex_home, &scope);
            if let Err(err) = store.reconcile_orphaned_in_flight(
                live_run_id.iter().map(String::as_str),
                now_unix_timestamp_ms() / 1000,
            ) {
                warn!(error = %err, "failed to reconcile durable auto review runs");
            }
        }
    }

    async fn record_pending_background_auto_review(
        &self,
        generation: u64,
        fingerprint: &str,
        persistence: ReviewPersistenceContext,
    ) -> bool {
        let mut state = self.state.lock().await;
        state
            .background_auto_review
            .record_pending(generation, fingerprint, persistence)
    }

    async fn record_skipped_background_auto_review(
        self: &Arc<Self>,
        persistence: &ReviewPersistenceContext,
        generation: u64,
        fingerprint: &str,
        error_summary: String,
    ) {
        let codex_home = self.codex_home().await;
        let mut state = self.state.lock().await;
        state
            .background_auto_review
            .clear_pending_review(generation, fingerprint);
        drop(state);
        if persistence.save_skipped(codex_home, error_summary.clone()) {
            record_background_review_status(
                Arc::clone(self),
                persistence,
                BackgroundAutoReviewStatus::Skipped,
                Some(error_summary),
            )
            .await;
        }
    }

    async fn record_skipped_duplicate_background_auto_review(
        self: &Arc<Self>,
        persistence: &ReviewPersistenceContext,
        generation: u64,
        fingerprint: &str,
        duplicate: &AutoReviewDuplicateMatch,
    ) {
        let codex_home = self.codex_home().await;
        let error_summary = format!(
            "equivalent background auto review already exists: {}",
            duplicate.run_id
        );
        let mut state = self.state.lock().await;
        state
            .background_auto_review
            .clear_pending_review(generation, fingerprint);
        drop(state);
        if persistence.save_skipped_duplicate(codex_home, duplicate) {
            record_background_review_status(
                Arc::clone(self),
                persistence,
                BackgroundAutoReviewStatus::Skipped,
                Some(error_summary),
            )
            .await;
        }
    }

    async fn resolve_durable_background_auto_review_duplicate(
        &self,
        persistence: &ReviewPersistenceContext,
    ) -> Option<AutoReviewDuplicateMatch> {
        let fingerprint = persistence.worktree_diff_fingerprint()?;
        let codex_home = self.codex_home().await;
        let store = AutoReviewStore::for_scope(codex_home, persistence.store_scope());
        let active_run_ids = {
            let state = self.state.lock().await;
            state.background_auto_review.active_snapshot()
        };
        let duplicate = match store.find_duplicate_by_fingerprint_with_target_proof_and_filter(
            fingerprint,
            Some(persistence.target()),
            Some(persistence.review_target()),
            |duplicate| match duplicate.disposition {
                AutoReviewDuplicateDisposition::ReuseTerminal => true,
                AutoReviewDuplicateDisposition::Adopt => {
                    active_run_ids.pending_run_id.as_deref() == Some(duplicate.run_id.as_str())
                        || active_run_ids.running_run_id.as_deref()
                            == Some(duplicate.run_id.as_str())
                }
                AutoReviewDuplicateDisposition::SupersedeTerminal => false,
            },
        ) {
            Ok(duplicate) => duplicate,
            Err(err) => {
                warn!(error = %err, "failed to query durable background auto review duplicates");
                None
            }
        }?;
        if duplicate.run_id == persistence.run_id() {
            return None;
        }
        Some(duplicate)
    }

    async fn supersede_clean_durable_background_auto_review_duplicates(
        &self,
        persistence: &ReviewPersistenceContext,
    ) {
        let Some(fingerprint) = persistence.worktree_diff_fingerprint() else {
            return;
        };
        let codex_home = self.codex_home().await;
        let store = AutoReviewStore::for_scope(codex_home, persistence.store_scope());
        match store.mark_superseded_by_fingerprint_with_target(
            fingerprint,
            persistence.run_id(),
            persistence.target().branch.as_deref(),
            persistence.target().head_sha.as_deref(),
            Some(persistence.review_target()),
        ) {
            Ok(changed) => {
                if changed > 0 {
                    debug!(
                        run_id = %persistence.run_id(),
                        changed,
                        "superseded clean durable background auto review duplicates"
                    );
                }
            }
            Err(err) => {
                warn!(error = %err, "failed to supersede durable background auto review duplicates");
            }
        }
    }

    async fn record_started_background_auto_review_if_idle(
        &self,
        generation: u64,
        fingerprint: &str,
        persistence: crate::review_persistence::ReviewPersistenceContext,
        coordination: &ReviewCoordination,
    ) -> anyhow::Result<Option<crate::state::BackgroundAutoReviewStart>> {
        let trigger_turn_mailbox = self.input_queue.lock_trigger_turn_mailbox_items().await;
        if trigger_turn_mailbox.has_trigger_turn_items() {
            return Ok(None);
        }
        let active_turn = self.active_turn.lock().await;
        if active_turn.is_some() {
            return Ok(None);
        }
        let mut state = self.state.lock().await;
        if !state.background_auto_review.is_current_schedule(generation) {
            return Ok(None);
        }
        let snapshot_epoch = coordination.bump_snapshot_epoch()?;
        let persistence = persistence.with_snapshot_epoch(snapshot_epoch);
        Ok(state
            .background_auto_review
            .record_started(generation, fingerprint, persistence))
    }

    pub(crate) async fn cancel_background_auto_review(self: &Arc<Self>) {
        self.cancel_background_auto_review_with_summary(
            AUTO_REVIEW_INTERRUPTED_ERROR_SUMMARY.to_string(),
        )
        .await;
    }

    pub(crate) async fn cancel_background_auto_review_for_foreground_work(self: &Arc<Self>) {
        self.cancel_background_auto_review_with_summary(background_auto_review_control_summary(
            &BackgroundAutoReviewControlAction::Cancel,
            &BackgroundAutoReviewControlReason::ForegroundWorkStarted,
        ))
        .await;
    }

    async fn cancel_background_auto_review_with_summary(self: &Arc<Self>, error_summary: String) {
        let cancellation = {
            let mut state = self.state.lock().await;
            state
                .background_auto_review
                .cancel_pending_and_take_reviews()
        };
        let codex_home = self.codex_home().await;
        if let Some(pending_review) = cancellation.pending_review
            && pending_review
                .persistence
                .save_cancelled_with_summary(&codex_home, error_summary.clone())
        {
            record_background_review_status(
                Arc::clone(self),
                &pending_review.persistence,
                BackgroundAutoReviewStatus::Cancelled,
                Some(error_summary.clone()),
            )
            .await;
        }
        let Some(running_review) = cancellation.running_review else {
            return;
        };
        self.cancel_running_background_auto_review(running_review, error_summary)
            .await;
    }

    pub(crate) async fn control_background_auto_review(
        self: &Arc<Self>,
        run_id: &str,
        action: BackgroundAutoReviewControlAction,
        reason: BackgroundAutoReviewControlReason,
    ) {
        let controlled_run = {
            let mut state = self.state.lock().await;
            state.background_auto_review.take_review_by_run_id(run_id)
        };
        let Some(controlled_run) = controlled_run else {
            debug!(%run_id, ?action, ?reason, "background auto review control ignored: run is not active");
            return;
        };
        let error_summary = background_auto_review_control_summary(&action, &reason);
        let superseded_by = background_auto_review_control_superseded_by(&reason);
        let codex_home = self.codex_home().await;
        match controlled_run {
            BackgroundAutoReviewControlledRun::Pending(pending_review) => {
                let status = background_auto_review_control_status(&action);
                let saved = match action {
                    BackgroundAutoReviewControlAction::Cancel => pending_review
                        .persistence
                        .save_cancelled_with_summary(codex_home, error_summary.clone()),
                    BackgroundAutoReviewControlAction::Supersede => {
                        pending_review.persistence.save_superseded_with_summary(
                            codex_home,
                            error_summary.clone(),
                            superseded_by,
                        )
                    }
                };
                if saved {
                    record_background_review_status(
                        Arc::clone(self),
                        &pending_review.persistence,
                        status,
                        Some(error_summary),
                    )
                    .await;
                }
            }
            BackgroundAutoReviewControlledRun::Running(running_review) => match action {
                BackgroundAutoReviewControlAction::Cancel => {
                    self.cancel_running_background_auto_review(running_review, error_summary)
                        .await;
                }
                BackgroundAutoReviewControlAction::Supersede => {
                    self.supersede_running_background_auto_review(
                        running_review,
                        error_summary,
                        superseded_by,
                    )
                    .await;
                }
            },
        }
    }

    async fn cancel_running_background_auto_review(
        self: &Arc<Self>,
        running_review: BackgroundAutoReviewRunningHandle,
        error_summary: String,
    ) {
        if running_review
            .persistence
            .save_cancelled_with_summary(self.codex_home().await, error_summary.clone())
        {
            record_background_review_status(
                Arc::clone(self),
                &running_review.persistence,
                BackgroundAutoReviewStatus::Cancelled,
                Some(error_summary),
            )
            .await;
        }
        let completion = running_review.completion;
        if completion.is_done() {
            return;
        }
        let notified = completion.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        running_review.cancellation_token.cancel();
        if completion.is_done() {
            return;
        }
        if tokio::time::timeout(Duration::from_millis(100), notified.as_mut())
            .await
            .is_err()
        {
            warn!("background auto review did not finish promptly after cancellation");
        }
    }

    async fn supersede_running_background_auto_review(
        self: &Arc<Self>,
        running_review: BackgroundAutoReviewRunningHandle,
        error_summary: String,
        superseded_by: Option<String>,
    ) {
        if running_review.persistence.save_superseded_with_summary(
            self.codex_home().await,
            error_summary.clone(),
            superseded_by,
        ) {
            record_background_review_status(
                Arc::clone(self),
                &running_review.persistence,
                BackgroundAutoReviewStatus::Superseded,
                Some(error_summary),
            )
            .await;
        }
        let completion = running_review.completion;
        if completion.is_done() {
            return;
        }
        let notified = completion.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        running_review.cancellation_token.cancel();
        if completion.is_done() {
            return;
        }
        if tokio::time::timeout(Duration::from_millis(100), notified.as_mut())
            .await
            .is_err()
        {
            warn!("background auto review did not finish promptly after supersede");
        }
    }

    pub(crate) async fn clear_background_auto_review(self: &Arc<Self>, generation: u64) {
        let mut state = self.state.lock().await;
        state
            .background_auto_review
            .clear_running_review(generation);
    }

    pub(crate) async fn clear_background_auto_review_turn(self: &Arc<Self>, turn_id: &str) {
        let mut state = self.state.lock().await;
        state.background_auto_review.remove_regular_turn(turn_id);
    }
}

fn auto_review_run_id_from_lock_intent(intent: &str) -> Option<String> {
    intent
        .strip_prefix("background_auto_review:")
        .or_else(|| intent.strip_prefix("manual_auto_review:"))
        .map(str::to_string)
}

async fn acquire_background_auto_review_lock(
    coordination: &ReviewCoordination,
    intent: String,
) -> anyhow::Result<Option<ReviewLockGuard>> {
    let deadline = tokio::time::Instant::now() + BACKGROUND_AUTO_REVIEW_LOCK_RETRY;
    loop {
        match coordination.try_acquire_lock(intent.clone())? {
            Some(guard) => return Ok(Some(guard)),
            None if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(BACKGROUND_AUTO_REVIEW_LOCK_RETRY_INTERVAL).await;
            }
            None => return Ok(None),
        }
    }
}

async fn background_review_fingerprint(turn_context: &TurnContext) -> Option<String> {
    let cwd = turn_context.environments.single_local_environment_cwd()?;
    background_review_fingerprint_for_cwd(cwd).await
}

fn background_auto_review_control_summary(
    action: &BackgroundAutoReviewControlAction,
    reason: &BackgroundAutoReviewControlReason,
) -> String {
    match (action, reason) {
        (
            BackgroundAutoReviewControlAction::Supersede,
            BackgroundAutoReviewControlReason::SupersededByRun { run_id },
        ) => format!("background auto review was superseded by run {run_id}"),
        (BackgroundAutoReviewControlAction::Supersede, _) => {
            "background auto review was superseded".to_string()
        }
        (
            BackgroundAutoReviewControlAction::Cancel,
            BackgroundAutoReviewControlReason::SupersededByRun { run_id },
        ) => format!("background auto review was cancelled after run {run_id} superseded it"),
        (_, BackgroundAutoReviewControlReason::UserRequested) => {
            "background auto review was cancelled by request".to_string()
        }
        (_, BackgroundAutoReviewControlReason::ForegroundWorkStarted) => {
            "foreground work started before background auto review could continue".to_string()
        }
        (_, BackgroundAutoReviewControlReason::ThreadClosing) => {
            "thread closed before background auto review could finish".to_string()
        }
    }
}

fn background_auto_review_control_status(
    action: &BackgroundAutoReviewControlAction,
) -> BackgroundAutoReviewStatus {
    match action {
        BackgroundAutoReviewControlAction::Cancel => BackgroundAutoReviewStatus::Cancelled,
        BackgroundAutoReviewControlAction::Supersede => BackgroundAutoReviewStatus::Superseded,
    }
}

fn background_auto_review_control_superseded_by(
    reason: &BackgroundAutoReviewControlReason,
) -> Option<String> {
    match reason {
        BackgroundAutoReviewControlReason::SupersededByRun { run_id } => Some(run_id.clone()),
        BackgroundAutoReviewControlReason::UserRequested
        | BackgroundAutoReviewControlReason::ForegroundWorkStarted
        | BackgroundAutoReviewControlReason::ThreadClosing => None,
    }
}

async fn background_review_fingerprint_for_cwd(cwd: &AbsolutePathBuf) -> Option<String> {
    get_worktree_diff_fingerprint(cwd.as_ref()).await
}

fn non_empty_turn_diff(diff: String) -> Option<String> {
    (!diff.trim().is_empty()).then_some(diff)
}

fn background_auto_review_turn_diff_prompt(turn_diff: &str) -> String {
    format!(
        "Review only the following unified diff from the just-completed turn. Do not review \
         unrelated pre-existing staged, unstaged, or untracked worktree changes except where \
         necessary to understand this diff. Provide prioritized findings.\n\n```diff\n{}\n```",
        turn_diff.trim()
    )
}
