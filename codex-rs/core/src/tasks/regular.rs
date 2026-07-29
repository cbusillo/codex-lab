use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::context::ContextualUserFragment;
use crate::context::ProjectValidationFailure;
use crate::session::TurnInput;
use crate::session::project_validation::ProjectValidationAttempt;
use crate::session::project_validation::ProjectValidationRun;
use crate::session::project_validation::project_validation_worktree_fingerprint;
use crate::session::project_validation::run_project_validation;
use crate::session::turn::ProjectValidationEligibility;
use crate::session::turn::RunTurnState;
use crate::session::turn::run_hooks_and_record_inputs;
use crate::session::turn::run_turn;
use crate::session::turn_context::TurnContext;
use crate::session_startup_prewarm::SessionStartupPrewarmResolution;
use crate::state::TaskKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnStartedEvent;
use tracing::Instrument;
use tracing::trace_span;

use super::SessionTask;
use super::SessionTaskContext;
use super::SessionTaskResult;

#[derive(Default)]
pub(crate) struct RegularTask;

/// Tracks where a turn sits in the bounded validate -> correct -> revalidate
/// cycle. Each root turn runs the project command at most twice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProjectValidationPhase {
    Initial,
    FollowUp,
    Correcting,
    Complete,
}

impl RegularTask {
    pub(crate) fn new() -> Self {
        Self
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

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        let sess = session.clone_session();
        let turn_extension_data = session.turn_extension_data();
        let run_turn_span = trace_span!("run_turn");
        // Regular turns emit `TurnStarted` inline so first-turn lifecycle does
        // not wait on startup prewarm resolution.
        let prewarmed_client_session = async {
            let event = EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: ctx.sub_id.clone(),
                trace_id: ctx.trace_id.clone(),
                started_at: ctx.turn_timing_state.started_at_unix_secs().await,
                model_context_window: ctx.model_context_window(),
                collaboration_mode_kind: ctx.mode,
            });
            sess.send_event(ctx.as_ref(), event).await;
            sess.set_server_reasoning_included(/*included*/ false).await;
            sess.consume_startup_prewarm_for_regular_turn(&cancellation_token)
                .await
        }
        .instrument(trace_span!("regular_task.prepare_run_turn"))
        .await;
        let prewarmed_client_session = match prewarmed_client_session {
            SessionStartupPrewarmResolution::Cancelled => {
                run_hooks_and_record_inputs(&sess, &ctx, &input).await;
                return Ok(None);
            }
            SessionStartupPrewarmResolution::Unavailable { .. } => None,
            SessionStartupPrewarmResolution::Ready(prewarmed_client_session) => {
                Some(*prewarmed_client_session)
            }
        };
        let mut next_input = input;
        let mut prewarmed_client_session = prewarmed_client_session;
        let mut project_validation_phase = ProjectValidationPhase::Initial;
        // Capture worktree state before any model work so validation can tell
        // turn-authored changes from pre-existing ones.
        let project_validation_worktree_at_turn_start = tokio::select! {
            _ = cancellation_token.cancelled() => return Ok(None),
            fingerprint = project_validation_worktree_fingerprint(&ctx) => fingerprint,
        };
        let mut project_validation_model_used_tools = false;
        let mut run_turn_state = RunTurnState::new();
        loop {
            let turn_result = run_turn(
                Arc::clone(&sess),
                Arc::clone(&ctx),
                Arc::clone(&turn_extension_data),
                next_input,
                &mut run_turn_state,
                prewarmed_client_session.take(),
                cancellation_token.child_token(),
            )
            .instrument(run_turn_span.clone())
            .await?;
            let last_agent_message = turn_result.last_agent_message.clone();
            let validation_eligible = turn_result.project_validation_eligibility
                == ProjectValidationEligibility::Eligible;
            project_validation_model_used_tools |= turn_result.model_used_tools;
            if !validation_eligible {
                return Ok(last_agent_message);
            }
            if sess.input_queue.has_pending_input(&sess.active_turn).await {
                next_input = Vec::new();
                continue;
            }
            let (attempt, may_start_correction, phase_after_validation) =
                match project_validation_phase {
                    ProjectValidationPhase::Initial => (
                        ProjectValidationAttempt::Initial {
                            worktree_at_turn_start: project_validation_worktree_at_turn_start
                                .clone(),
                            model_used_tools: project_validation_model_used_tools,
                        },
                        true,
                        ProjectValidationPhase::FollowUp,
                    ),
                    ProjectValidationPhase::FollowUp => (
                        ProjectValidationAttempt::Initial {
                            worktree_at_turn_start: project_validation_worktree_at_turn_start
                                .clone(),
                            model_used_tools: project_validation_model_used_tools,
                        },
                        false,
                        ProjectValidationPhase::Complete,
                    ),
                    ProjectValidationPhase::Correcting => (
                        ProjectValidationAttempt::CorrectionRerun {
                            worktree_at_turn_start: project_validation_worktree_at_turn_start
                                .clone(),
                        },
                        false,
                        ProjectValidationPhase::Complete,
                    ),
                    ProjectValidationPhase::Complete => return Ok(last_agent_message),
                };
            let validation_event = match run_project_validation(
                &sess,
                &ctx,
                attempt,
                cancellation_token.child_token(),
            )
            .await
            {
                ProjectValidationRun::Skipped(event) => event,
                ProjectValidationRun::Completed(event) => {
                    if may_start_correction
                        && let Some(correction) = ProjectValidationFailure::from_event(&event)
                    {
                        sess.send_event(&ctx, EventMsg::ProjectValidationCompleted(event))
                            .await;
                        if cancellation_token.is_cancelled() {
                            return Ok(None);
                        }
                        let correction_item = ContextualUserFragment::into(correction);
                        sess.record_conversation_items(
                            &ctx,
                            std::slice::from_ref(&correction_item),
                        )
                        .await;
                        project_validation_phase = ProjectValidationPhase::Correcting;
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
            project_validation_phase = phase_after_validation;
            sess.send_event(&ctx, EventMsg::ProjectValidationCompleted(validation_event))
                .await;
            if sess.input_queue.has_pending_input(&sess.active_turn).await {
                next_input = Vec::new();
                continue;
            }
            return Ok(last_agent_message);
        }
    }
}
