use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::session::TurnInput;
use crate::session::project_validation::ProjectValidationRun;
use crate::session::project_validation::run_project_validation;
use crate::session::turn::ProjectValidationEligibility;
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

#[derive(Default)]
pub(crate) struct RegularTask;

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
    ) -> Option<String> {
        let sess = session.clone_session();
        let turn_extension_data = session.turn_extension_data();
        let run_turn_span = trace_span!("run_turn");
        // Regular turns emit `TurnStarted` inline so first-turn lifecycle does
        // not wait on startup prewarm resolution.
        let event = EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: ctx.sub_id.clone(),
            trace_id: ctx.trace_id.clone(),
            started_at: ctx.turn_timing_state.started_at_unix_secs().await,
            model_context_window: ctx.model_context_window(),
            collaboration_mode_kind: ctx.collaboration_mode.mode,
        });
        sess.send_event(ctx.as_ref(), event).await;
        sess.set_server_reasoning_included(/*included*/ false).await;
        let prewarmed_client_session = match sess
            .consume_startup_prewarm_for_regular_turn(&cancellation_token)
            .await
        {
            SessionStartupPrewarmResolution::Cancelled => return None,
            SessionStartupPrewarmResolution::Unavailable { .. } => None,
            SessionStartupPrewarmResolution::Ready(prewarmed_client_session) => {
                Some(*prewarmed_client_session)
            }
        };
        let mut next_input = input;
        let mut prewarmed_client_session = prewarmed_client_session;
        let mut project_validation_has_run = false;
        loop {
            let turn_result = run_turn(
                Arc::clone(&sess),
                Arc::clone(&ctx),
                Arc::clone(&turn_extension_data),
                next_input,
                prewarmed_client_session.take(),
                cancellation_token.child_token(),
            )
            .instrument(run_turn_span.clone())
            .await;
            let last_agent_message = turn_result
                .as_ref()
                .and_then(|result| result.last_agent_message.clone());
            let validation_eligible = turn_result.as_ref().is_some_and(|result| {
                result.project_validation_eligibility == ProjectValidationEligibility::Eligible
            });
            if sess.input_queue.has_pending_input(&sess.active_turn).await {
                next_input = Vec::new();
                continue;
            }
            if validation_eligible && !project_validation_has_run {
                project_validation_has_run = true;
                match run_project_validation(&sess, &ctx, cancellation_token.child_token()).await {
                    ProjectValidationRun::Skipped => {}
                    ProjectValidationRun::Completed(event) => {
                        sess.send_event(&ctx, EventMsg::ProjectValidationCompleted(event))
                            .await;
                    }
                    ProjectValidationRun::Cancelled => return None,
                }
                if sess.input_queue.has_pending_input(&sess.active_turn).await {
                    next_input = Vec::new();
                    continue;
                }
            }
            return last_agent_message;
        }
    }
}
