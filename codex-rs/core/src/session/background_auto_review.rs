use std::sync::Arc;
use std::time::Duration;

use codex_git_utils::get_worktree_diff_fingerprint;
use codex_protocol::protocol::ReviewPersistence;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_utils_absolute_path::AbsolutePathBuf;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::warn;

use super::review::prepare_review_thread;
use super::review::spawn_detached_review_thread;
use super::session::Session;
use super::turn_context::TurnContext;
use crate::review_prompts::resolve_review_request;

const BACKGROUND_AUTO_REVIEW_DEBOUNCE: Duration = Duration::from_secs(2);

impl Session {
    pub(crate) async fn record_background_auto_review_turn_start(
        self: &Arc<Self>,
        turn_context: &TurnContext,
    ) {
        let mut state = self.state.lock().await;
        state
            .background_auto_review
            .begin_regular_turn(turn_context.sub_id.clone());

        let Some(cwd) = turn_context
            .environments
            .single_local_environment_cwd()
            .cloned()
        else {
            return;
        };
        let sess = Arc::clone(self);
        let turn_id = turn_context.sub_id.clone();
        tokio::spawn(async move {
            let fingerprint = background_review_fingerprint_for_cwd(&cwd).await;
            let mut state = sess.state.lock().await;
            state
                .background_auto_review
                .update_regular_turn_start_fingerprint(&turn_id, fingerprint);
        });
    }

    pub(crate) async fn maybe_schedule_background_auto_review(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
    ) {
        let Some(after_fingerprint) = background_review_fingerprint(turn_context.as_ref()).await
        else {
            let mut state = self.state.lock().await;
            let _ = state
                .background_auto_review
                .complete_regular_turn(&turn_context.sub_id, None);
            debug!(turn_id = %turn_context.sub_id, "background auto review skipped: clean or unsupported worktree");
            return;
        };
        let schedule = {
            let mut state = self.state.lock().await;
            state
                .background_auto_review
                .complete_regular_turn(&turn_context.sub_id, Some(after_fingerprint))
        };
        let Some(schedule) = schedule else {
            debug!(turn_id = %turn_context.sub_id, "background auto review skipped: unchanged or duplicate fingerprint");
            return;
        };

        let sess = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(BACKGROUND_AUTO_REVIEW_DEBOUNCE).await;
            if !sess
                .is_current_background_auto_review_schedule(
                    schedule.generation,
                    &schedule.fingerprint,
                )
                .await
            {
                debug!("background auto review debounce superseded");
                return;
            }
            let Some(current_fingerprint) =
                background_review_fingerprint(turn_context.as_ref()).await
            else {
                debug!(
                    "background auto review skipped after debounce: clean or unsupported worktree"
                );
                return;
            };
            if current_fingerprint != schedule.fingerprint {
                debug!("background auto review skipped after debounce: fingerprint changed");
                return;
            }
            sess.start_detached_background_auto_review(
                turn_context,
                schedule.generation,
                schedule.fingerprint,
            )
            .await;
        });
    }

    async fn is_current_background_auto_review_schedule(
        &self,
        generation: u64,
        fingerprint: &str,
    ) -> bool {
        let state = self.state.lock().await;
        state
            .background_auto_review
            .is_current_schedule(generation, fingerprint)
    }

    async fn start_detached_background_auto_review(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        generation: u64,
        fingerprint: String,
    ) {
        if self.input_queue.has_trigger_turn_mailbox_items().await
            || self.active_turn.lock().await.is_some()
        {
            debug!("background auto review skipped: foreground work pending or active");
            return;
        }

        let Some(cwd) = turn_context.environments.single_local_environment_cwd() else {
            debug!("background auto review skipped: no single local worktree");
            return;
        };
        let sub_id = uuid::Uuid::new_v4().to_string();
        let review_request = ReviewRequest {
            target: ReviewTarget::UncommittedChanges,
            user_facing_hint: None,
        };
        let resolved = match resolve_review_request(review_request, cwd) {
            Ok(resolved) => resolved,
            Err(err) => {
                warn!(error = %err, "background auto review request resolution failed");
                return;
            }
        };
        let prepared = prepare_review_thread(
            Arc::clone(self),
            Arc::clone(&turn_context.config),
            turn_context,
            sub_id,
            resolved,
            Some(ReviewPersistence::BackgroundAutoReview),
        )
        .await;
        let Some(cancellation_token) = self
            .record_started_background_auto_review(generation, &fingerprint)
            .await
        else {
            debug!("background auto review skipped after prepare: schedule superseded");
            return;
        };
        if self.input_queue.has_trigger_turn_mailbox_items().await
            || self.active_turn.lock().await.is_some()
        {
            debug!(
                "background auto review skipped after prepare: foreground work pending or active"
            );
            cancellation_token.cancel();
            self.clear_background_auto_review(generation).await;
            return;
        }
        spawn_detached_review_thread(Arc::clone(self), prepared, cancellation_token, generation);
    }

    async fn record_started_background_auto_review(
        &self,
        generation: u64,
        fingerprint: &str,
    ) -> Option<CancellationToken> {
        let mut state = self.state.lock().await;
        state
            .background_auto_review
            .record_started(generation, fingerprint)
    }

    pub(crate) async fn cancel_background_auto_review(self: &Arc<Self>) {
        let mut state = self.state.lock().await;
        state.background_auto_review.cancel_running_review();
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

async fn background_review_fingerprint(turn_context: &TurnContext) -> Option<String> {
    let cwd = turn_context.environments.single_local_environment_cwd()?;
    background_review_fingerprint_for_cwd(cwd).await
}

async fn background_review_fingerprint_for_cwd(cwd: &AbsolutePathBuf) -> Option<String> {
    get_worktree_diff_fingerprint(cwd.as_ref()).await
}
