use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use codex_analytics::TurnProfileFact;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use futures::FutureExt;
use tracing::warn;

use crate::hook_runtime::run_turn_interrupt_hooks;
use crate::session::session::Session;
use crate::state::ActiveTurn;
use crate::state::RunningTask;

use super::InterruptedTurnHistoryMarker;
use super::interrupted_turn_history_marker;

pub(super) async fn finalize_aborted_turn(
    session: Arc<Session>,
    task: RunningTask,
    active_turn: ActiveTurn,
    reason: TurnAbortReason,
    interrupt_generation: Option<u64>,
) {
    let sub_id = task.turn_context.sub_id.clone();
    if AssertUnwindSafe(task.start.run())
        .catch_unwind()
        .await
        .is_err()
    {
        warn!(sub_id, "task start lifecycle panicked during abort");
    }
    if AssertUnwindSafe(
        task.task
            .abort(Arc::clone(&session), Arc::clone(&task.turn_context)),
    )
    .catch_unwind()
    .await
    .is_err()
    {
        warn!(sub_id, "task abort cleanup panicked");
    }

    if reason == TurnAbortReason::Interrupted
        && let Some(marker) =
            interrupted_turn_history_marker(InterruptedTurnHistoryMarker::from_config_and_version(
                task.turn_context.config.as_ref(),
                task.turn_context.multi_agent_version,
            ))
    {
        session
            .record_conversation_items(task.turn_context.as_ref(), std::slice::from_ref(&marker))
            .await;
        if let Err(err) = session.flush_rollout().await {
            warn!("failed to flush interrupted-turn marker before emitting TurnAborted: {err}");
        }
    }

    if reason == TurnAbortReason::Interrupted {
        run_turn_interrupt_hooks(&session, &task.turn_context, &active_turn.turn_state).await;
    }

    let started_at = task
        .turn_context
        .turn_timing_state
        .started_at_unix_secs()
        .await;
    let (completed_at, duration_ms, profile) = task
        .turn_context
        .turn_timing_state
        .complete_profile_and_duration_ms()
        .await;
    session
        .services
        .analytics_events_client
        .track_turn_profile(TurnProfileFact {
            turn_id: task.turn_context.sub_id.clone(),
            profile,
        });
    let event = EventMsg::TurnAborted(TurnAbortedEvent {
        turn_id: Some(task.turn_context.sub_id.clone()),
        reason: reason.clone(),
        started_at,
        completed_at,
        duration_ms,
    });
    session.send_event(task.turn_context.as_ref(), event).await;
    session
        .services
        .guardian_rejection_circuit_breaker
        .lock()
        .await
        .clear_turn(&task.turn_context.sub_id);
    if let Err(err) = session.flush_rollout().await {
        warn!("failed to flush rollout after emitting terminal turn event: {err}");
    }
    session
        .emit_turn_abort_lifecycle(reason, task.turn_context.extension_data.as_ref())
        .await;
    session.input_queue.clear_pending(&active_turn).await;
    if let Some(generation) = interrupt_generation {
        session
            .maybe_start_turn_for_pending_work_after_interrupt(generation)
            .await;
    }
}
