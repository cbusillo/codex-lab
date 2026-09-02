use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::context::ContextualUserFragment;
use crate::context::ProjectValidationCorrectionConsumed;
use crate::context::ProjectValidationFailure;
use crate::context_manager::ModelRequestHistoryMode;
use crate::session::TurnInput;
use crate::session::project_validation::ProjectValidationAttempt;
use crate::session::project_validation::ProjectValidationRun;
use crate::session::project_validation::project_validation_worktree_fingerprint;
use crate::session::project_validation::run_project_validation;
use crate::session::session::Session;
use crate::session::turn::InitialInputRecorder;
use crate::session::turn::ProjectValidationEligibility;
use crate::session::turn::RunTurnParams;
use crate::session::turn::RunTurnState;
use crate::session::turn::run_turn;
use crate::session::turn_context::TurnContext;
use crate::session_startup_prewarm::SessionStartupPrewarmResolution;
use crate::state::TaskKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnStartedEvent;
use tracing::Instrument;
use tracing::trace_span;

use super::SessionTask;
use super::SessionTaskResult;

pub(crate) struct RegularTask {
    initial_input_recorder: Arc<InitialInputRecorder>,
}

/// Tracks whether the next validation follows ordinary model work or the
/// single corrective model run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NextProjectValidationAttempt {
    Initial,
    CorrectionRerun,
}

impl RegularTask {
    pub(crate) fn new(input: &[TurnInput]) -> Self {
        Self {
            initial_input_recorder: Arc::new(InitialInputRecorder::new(input)),
        }
    }
}

impl SessionTask for RegularTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.turn"
    }

    fn background_review_trigger_eligible(&self) -> bool {
        true
    }

    async fn start(&self, sess: Arc<Session>, ctx: Arc<TurnContext>) {
        let event = EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: ctx.sub_id.clone(),
            trace_id: ctx.trace_id.clone(),
            started_at: ctx.turn_timing_state.started_at_unix_secs().await,
            model_context_window: ctx.model_context_window(),
            collaboration_mode_kind: ctx.mode,
        });
        sess.send_event(ctx.as_ref(), event).await;
        sess.set_server_reasoning_included(/*included*/ false).await;
    }

    async fn run(
        self: Arc<Self>,
        sess: Arc<Session>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        let run_turn_span = trace_span!("run_turn");
        // `SessionTask::start` emits `TurnStarted` before startup prewarm resolution.
        let prewarmed_client_session = sess
            .consume_startup_prewarm_for_regular_turn(&cancellation_token)
            .instrument(trace_span!("regular_task.prepare_run_turn"))
            .await;
        let prewarmed_client_session = match prewarmed_client_session {
            SessionStartupPrewarmResolution::Cancelled => {
                self.initial_input_recorder
                    .record(Arc::clone(&sess), Arc::clone(&ctx))
                    .await;
                return Ok(None);
            }
            SessionStartupPrewarmResolution::Unavailable { .. } => None,
            SessionStartupPrewarmResolution::Ready(prewarmed_client_session) => {
                Some(*prewarmed_client_session)
            }
        };
        let mut next_input = input;
        let mut prewarmed_client_session = prewarmed_client_session;
        let mut next_project_validation_attempt = NextProjectValidationAttempt::Initial;
        let mut correction_available = true;
        // Capture worktree state before any model work so validation can tell
        // turn-authored changes from pre-existing ones.
        let project_validation_worktree_at_turn_start = tokio::select! {
            _ = cancellation_token.cancelled() => {
                self.initial_input_recorder
                    .record(Arc::clone(&sess), Arc::clone(&ctx))
                    .await;
                return Ok(None);
            },
            fingerprint = project_validation_worktree_fingerprint(&ctx) => fingerprint,
        };
        let mut project_validation_model_used_tools = false;
        let mut run_turn_state = RunTurnState::new();
        loop {
            let model_request_history_mode = match next_project_validation_attempt {
                NextProjectValidationAttempt::Initial => ModelRequestHistoryMode::Normal,
                NextProjectValidationAttempt::CorrectionRerun => {
                    ModelRequestHistoryMode::ProjectValidationCorrection
                }
            };
            let turn_result = run_turn(
                RunTurnParams {
                    sess: Arc::clone(&sess),
                    turn_context: Arc::clone(&ctx),
                    turn_extension_data: Arc::clone(&ctx.extension_data),
                    input: next_input,
                    model_request_history_mode,
                    prewarmed_client_session: prewarmed_client_session.take(),
                    cancellation_token: cancellation_token.child_token(),
                    initial_input_recorder: Arc::clone(&self.initial_input_recorder),
                },
                &mut run_turn_state,
            )
            .instrument(run_turn_span.clone())
            .await;
            let turn_result = turn_result?;
            let last_agent_message = turn_result.last_agent_message.clone();
            let validation_eligible = turn_result.project_validation_eligibility
                == ProjectValidationEligibility::Eligible;
            project_validation_model_used_tools |= turn_result.model_used_tools;
            if sess.input_queue.has_pending_input(&sess.active_turn).await {
                next_input = Vec::new();
                continue;
            }
            if !validation_eligible {
                return Ok(last_agent_message);
            }
            let attempt = match next_project_validation_attempt {
                NextProjectValidationAttempt::Initial => ProjectValidationAttempt::Initial {
                    worktree_at_turn_start: project_validation_worktree_at_turn_start.clone(),
                    model_used_tools: project_validation_model_used_tools,
                },
                NextProjectValidationAttempt::CorrectionRerun => {
                    ProjectValidationAttempt::CorrectionRerun {
                        worktree_at_turn_start: project_validation_worktree_at_turn_start.clone(),
                    }
                }
            };
            let validation_event = match run_project_validation(
                &sess,
                &ctx,
                attempt,
                cancellation_token.child_token(),
            )
            .await
            {
                ProjectValidationRun::NotApplicable => return Ok(last_agent_message),
                ProjectValidationRun::Skipped(event) => event,
                ProjectValidationRun::Completed(event) => {
                    if correction_available
                        && let Some(correction) = ProjectValidationFailure::from_event(&event)
                    {
                        sess.send_event(&ctx, EventMsg::ProjectValidationCompleted(event))
                            .await;
                        if cancellation_token.is_cancelled() {
                            return Ok(None);
                        }
                        let correction_item = ContextualUserFragment::into(correction);
                        let correction_consumed_item =
                            ContextualUserFragment::into(ProjectValidationCorrectionConsumed);
                        sess.record_conversation_items(
                            &ctx,
                            &[correction_item, correction_consumed_item],
                        )
                        .await;
                        sess.flush_rollout().await?;
                        correction_available = false;
                        next_project_validation_attempt =
                            NextProjectValidationAttempt::CorrectionRerun;
                        next_input = Vec::new();
                        continue;
                    }
                    event
                }
                ProjectValidationRun::Cancelled(event) => {
                    sess.send_event(&ctx, EventMsg::ProjectValidationCompleted(event))
                        .await;
                    return Ok(None);
                }
            };
            next_project_validation_attempt = NextProjectValidationAttempt::Initial;
            sess.send_event(&ctx, EventMsg::ProjectValidationCompleted(validation_event))
                .await;
            if sess.input_queue.has_pending_input(&sess.active_turn).await {
                next_input = Vec::new();
                continue;
            }
            return Ok(last_agent_message);
        }
    }

    fn abort(
        &self,
        sess: Arc<Session>,
        ctx: Arc<TurnContext>,
    ) -> impl std::future::Future<Output = ()> + Send {
        let initial_input_recorder = Arc::clone(&self.initial_input_recorder);
        async move {
            initial_input_recorder.record(sess, ctx).await;
        }
    }
}
