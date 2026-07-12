//! Storage-neutral projection from paginated rollout records to history mutations.
//!
//! This contract is intentionally not wired to storage or app-server request handling yet.

use crate::protocol::thread_history::render_review_output_text;
use crate::protocol::v2::ThreadItem;
use crate::protocol::v2::TurnError;
use crate::protocol::v2::TurnStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadHistoryProjectionChangeSet {
    ordinal: i64,
    mutation: Option<ThreadHistoryProjectionMutation>,
}

impl ThreadHistoryProjectionChangeSet {
    fn new(ordinal: i64, mutation: Option<ThreadHistoryProjectionMutation>) -> Self {
        Self { ordinal, mutation }
    }

    pub fn ordinal(&self) -> i64 {
        self.ordinal
    }

    pub fn mutation(&self) -> Option<&ThreadHistoryProjectionMutation> {
        self.mutation.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.mutation.is_none()
    }

    pub fn insertion_ordinal(&self, existing: Option<i64>) -> i64 {
        existing.unwrap_or(self.ordinal)
    }

    pub fn into_parts(self) -> (i64, Option<ThreadHistoryProjectionMutation>) {
        (self.ordinal, self.mutation)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ThreadHistoryProjectionMutation {
    UpsertTurn {
        target: ThreadHistoryTurnTarget,
        update: ThreadHistoryTurnUpdate,
    },
    UpsertItem {
        target: ThreadHistoryTurnTarget,
        item: ThreadItem,
    },
    RemoveLatestTurns {
        count: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadHistoryTurnTarget {
    Id(String),
    Active,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadHistoryTurnUpdate {
    pub status: TurnStatus,
    pub error: Option<TurnError>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ThreadHistoryProjectionError {
    #[error("paginated rollout line is missing an ordinal")]
    MissingOrdinal,
    #[error("paginated rollout ordinal {0} exceeds SQLite's signed integer range")]
    OrdinalOverflow(u64),
}

pub fn project_rollout_line(
    line: &RolloutLine,
) -> Result<ThreadHistoryProjectionChangeSet, ThreadHistoryProjectionError> {
    let ordinal = line
        .ordinal
        .ok_or(ThreadHistoryProjectionError::MissingOrdinal)
        .and_then(|ordinal| {
            i64::try_from(ordinal)
                .map_err(|_| ThreadHistoryProjectionError::OrdinalOverflow(ordinal))
        })?;
    let mutation = match &line.item {
        RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => {
            Some(ThreadHistoryProjectionMutation::UpsertTurn {
                target: ThreadHistoryTurnTarget::Id(event.turn_id.clone()),
                update: ThreadHistoryTurnUpdate {
                    status: TurnStatus::InProgress,
                    error: None,
                    started_at: event.started_at,
                    completed_at: None,
                    duration_ms: None,
                },
            })
        }
        RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => {
            Some(ThreadHistoryProjectionMutation::UpsertTurn {
                target: ThreadHistoryTurnTarget::Id(event.turn_id.clone()),
                update: ThreadHistoryTurnUpdate {
                    status: if event.error.is_some() {
                        TurnStatus::Failed
                    } else {
                        TurnStatus::Completed
                    },
                    error: event.error.as_ref().map(|error| TurnError {
                        message: error.message.clone(),
                        codex_error_info: error.codex_error_info.clone().map(Into::into),
                        additional_details: None,
                    }),
                    started_at: event.started_at,
                    completed_at: event.completed_at,
                    duration_ms: event.duration_ms,
                },
            })
        }
        RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => {
            Some(ThreadHistoryProjectionMutation::UpsertTurn {
                target: event
                    .turn_id
                    .clone()
                    .map_or(ThreadHistoryTurnTarget::Active, ThreadHistoryTurnTarget::Id),
                update: ThreadHistoryTurnUpdate {
                    status: TurnStatus::Interrupted,
                    error: None,
                    started_at: event.started_at,
                    completed_at: event.completed_at,
                    duration_ms: event.duration_ms,
                },
            })
        }
        RolloutItem::EventMsg(EventMsg::ItemCompleted(event)) => {
            Some(ThreadHistoryProjectionMutation::UpsertItem {
                target: ThreadHistoryTurnTarget::Id(event.turn_id.clone()),
                item: ThreadItem::from(event.item.clone()),
            })
        }
        RolloutItem::EventMsg(EventMsg::EnteredReviewMode(event)) => {
            Some(ThreadHistoryProjectionMutation::UpsertItem {
                target: ThreadHistoryTurnTarget::Active,
                item: ThreadItem::EnteredReviewMode {
                    id: review_item_id(ReviewItemKind::Entered, ordinal),
                    review: event
                        .user_facing_hint
                        .clone()
                        .unwrap_or_else(|| "Review requested.".to_string()),
                },
            })
        }
        RolloutItem::EventMsg(EventMsg::ExitedReviewMode(event)) => {
            Some(ThreadHistoryProjectionMutation::UpsertItem {
                target: ThreadHistoryTurnTarget::Active,
                item: ThreadItem::ExitedReviewMode {
                    id: review_item_id(ReviewItemKind::Exited, ordinal),
                    review: event
                        .review_output
                        .as_ref()
                        .map(render_review_output_text)
                        .unwrap_or_else(|| "Reviewer failed to output a response.".to_string()),
                },
            })
        }
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(event)) => {
            Some(ThreadHistoryProjectionMutation::RemoveLatestTurns {
                count: event.num_turns,
            })
        }
        RolloutItem::SessionMeta(_)
        | RolloutItem::ResponseItem(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::TurnContext(_)
        | RolloutItem::EventMsg(_) => None,
    };

    Ok(ThreadHistoryProjectionChangeSet::new(ordinal, mutation))
}

#[derive(Clone, Copy)]
enum ReviewItemKind {
    Entered,
    Exited,
}

fn review_item_id(kind: ReviewItemKind, ordinal: i64) -> String {
    let kind = match kind {
        ReviewItemKind::Entered => "entered",
        ReviewItemKind::Exited => "exited",
    };
    format!("review-{kind}-{ordinal}")
}

#[cfg(test)]
#[path = "thread_history_projection_tests.rs"]
mod tests;
