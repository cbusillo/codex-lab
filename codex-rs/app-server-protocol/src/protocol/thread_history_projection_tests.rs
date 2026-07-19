use super::*;
use crate::protocol::v2::CodexErrorInfo;
use codex_protocol::ThreadId;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::ExitedReviewModeEvent;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationSkipReason as CoreProjectValidationSkipReason;
use codex_protocol::protocol::ProjectValidationStatus as CoreProjectValidationStatus;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::protocol::WarningEvent;
use pretty_assertions::assert_eq;

#[test]
fn projects_turn_lifecycle_with_terminal_times() {
    let started = project(
        3,
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            trace_id: None,
            started_at: Some(10),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
    )
    .expect("started projection");
    let completed = project(
        9,
        EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
            error: None,
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
            time_to_first_token_ms: None,
        }),
    )
    .expect("completed projection");

    assert_eq!(
        started.mutation(),
        Some(&ThreadHistoryProjectionMutation::UpsertTurn {
            target: ThreadHistoryTurnTarget::Id("turn-1".to_string()),
            update: ThreadHistoryTurnUpdate {
                status: TurnStatus::InProgress,
                error: None,
                started_at: Some(10),
                completed_at: None,
                duration_ms: None,
            },
        })
    );
    assert_eq!(
        completed.mutation(),
        Some(&ThreadHistoryProjectionMutation::UpsertTurn {
            target: ThreadHistoryTurnTarget::Id("turn-1".to_string()),
            update: ThreadHistoryTurnUpdate {
                status: TurnStatus::Completed,
                error: None,
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
            },
        })
    );
    assert_eq!(completed.insertion_ordinal(Some(started.ordinal())), 3);
}

#[test]
fn projects_failed_turn_completion_with_terminal_error() {
    let changes = project(
        11,
        EventMsg::TurnComplete(TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
            error: Some(ErrorEvent {
                message: "stream failed".to_string(),
                codex_error_info: Some(
                    codex_protocol::protocol::CodexErrorInfo::ResponseStreamDisconnected {
                        http_status_code: Some(502),
                    },
                ),
            }),
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
            time_to_first_token_ms: None,
        }),
    )
    .expect("failed projection");

    assert_eq!(
        changes.mutation(),
        Some(&ThreadHistoryProjectionMutation::UpsertTurn {
            target: ThreadHistoryTurnTarget::Id("turn-1".to_string()),
            update: ThreadHistoryTurnUpdate {
                status: TurnStatus::Failed,
                error: Some(TurnError {
                    message: "stream failed".to_string(),
                    codex_error_info: Some(CodexErrorInfo::ResponseStreamDisconnected {
                        http_status_code: Some(502),
                    }),
                    additional_details: None,
                }),
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
            },
        })
    );
}

#[test]
fn projects_identified_and_idless_turn_aborts() {
    let identified = project(
        4,
        EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id: Some("turn-1".to_string()),
            reason: TurnAbortReason::Interrupted,
            started_at: Some(10),
            completed_at: Some(12),
            duration_ms: Some(2_000),
        }),
    )
    .expect("identified abort projection");
    let idless = project(
        5,
        EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id: None,
            reason: TurnAbortReason::Interrupted,
            started_at: Some(10),
            completed_at: Some(12),
            duration_ms: Some(2_000),
        }),
    )
    .expect("idless abort projection");

    assert!(matches!(
        identified.mutation(),
        Some(ThreadHistoryProjectionMutation::UpsertTurn {
            target: ThreadHistoryTurnTarget::Id(turn_id),
            ..
        }) if turn_id == "turn-1"
    ));
    assert_eq!(
        idless.mutation(),
        Some(&ThreadHistoryProjectionMutation::UpsertTurn {
            target: ThreadHistoryTurnTarget::Active,
            update: ThreadHistoryTurnUpdate {
                status: TurnStatus::Interrupted,
                error: None,
                started_at: Some(10),
                completed_at: Some(12),
                duration_ms: Some(2_000),
            },
        })
    );
}

#[test]
fn preserves_canonical_item_ids_and_first_ordinals_on_updates() {
    let initial = completed_item(
        12,
        TurnItem::AgentMessage(AgentMessageItem {
            id: "msg_00000000-0000-7000-8000-000000000001".to_string(),
            content: vec![AgentMessageContent::Text {
                text: "first".to_string(),
            }],
            phase: None,
            memory_citation: None,
        }),
    )
    .expect("initial item projection");
    let updated = completed_item(
        20,
        TurnItem::AgentMessage(AgentMessageItem {
            id: "msg_00000000-0000-7000-8000-000000000001".to_string(),
            content: vec![AgentMessageContent::Text {
                text: "updated".to_string(),
            }],
            phase: None,
            memory_citation: None,
        }),
    )
    .expect("updated item projection");

    let Some(ThreadHistoryProjectionMutation::UpsertItem {
        item: initial_item, ..
    }) = initial.mutation()
    else {
        panic!("expected initial item upsert");
    };
    let Some(ThreadHistoryProjectionMutation::UpsertItem {
        item: updated_item, ..
    }) = updated.mutation()
    else {
        panic!("expected updated item upsert");
    };
    assert_eq!(
        initial_item.id(),
        "msg_00000000-0000-7000-8000-000000000001"
    );
    assert_eq!(updated_item.id(), initial_item.id());
    assert_eq!(updated.insertion_ordinal(Some(initial.ordinal())), 12);
}

#[test]
fn ignores_item_started_until_a_canonical_completion_exists() {
    let changes = project(
        13,
        EventMsg::ItemStarted(ItemStartedEvent {
            thread_id: ThreadId::default(),
            turn_id: "turn-1".to_string(),
            item: TurnItem::AgentMessage(AgentMessageItem {
                id: "msg_00000000-0000-7000-8000-000000000002".to_string(),
                content: vec![AgentMessageContent::Text {
                    text: "partial".to_string(),
                }],
                phase: None,
                memory_citation: None,
            }),
            started_at_ms: 123,
        }),
    )
    .expect("started item projection");

    assert!(changes.is_empty());
}

#[test]
fn review_items_use_stable_ordinal_derived_ids() {
    let entered_line = rollout_line(
        Some(40),
        EventMsg::EnteredReviewMode(ReviewRequest {
            target: ReviewTarget::Custom {
                instructions: "review this".to_string(),
            },
            user_facing_hint: Some("Review requested by the user.".to_string()),
        }),
    );
    let entered = project_rollout_line(&entered_line).expect("entered review projection");
    let entered_again = project_rollout_line(&entered_line).expect("replayed review projection");
    let exited = project(
        44,
        EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
            review_output: Some(ReviewOutputEvent {
                overall_explanation: "No findings.".to_string(),
                ..Default::default()
            }),
        }),
    )
    .expect("exited review projection");

    assert_eq!(entered, entered_again);
    assert_eq!(
        entered.mutation(),
        Some(&ThreadHistoryProjectionMutation::UpsertItem {
            target: ThreadHistoryTurnTarget::Active,
            item: ThreadItem::EnteredReviewMode {
                id: "review-entered-40".to_string(),
                review: "Review requested by the user.".to_string(),
            },
        })
    );
    assert_eq!(
        exited.mutation(),
        Some(&ThreadHistoryProjectionMutation::UpsertItem {
            target: ThreadHistoryTurnTarget::Active,
            item: ThreadItem::ExitedReviewMode {
                id: "review-exited-44".to_string(),
                review: "No findings.".to_string(),
            },
        })
    );
}

#[test]
fn project_validation_uses_stable_ordinal_derived_item_id() {
    let changes = project(
        45,
        EventMsg::ProjectValidationCompleted(ProjectValidationCompletedEvent {
            turn_id: "turn-1".to_string(),
            command: vec!["shellcheck".to_string()],
            command_truncated: false,
            cwd: None,
            status: CoreProjectValidationStatus::Skipped,
            skip_reason: Some(CoreProjectValidationSkipReason::NoApplicableProvider),
            changed_file_count: Some(2),
            exit_code: None,
            output: "automatic validation skipped".to_string(),
            output_truncated: false,
            duration_ms: 0,
        }),
    )
    .expect("project validation projection");

    assert_eq!(
        changes.mutation(),
        Some(&ThreadHistoryProjectionMutation::UpsertItem {
            target: ThreadHistoryTurnTarget::Id("turn-1".to_string()),
            item: ThreadItem::ProjectValidation {
                id: "project-validation-45".to_string(),
                command: vec!["shellcheck".to_string()],
                command_truncated: false,
                cwd: None,
                status: crate::protocol::v2::ProjectValidationStatus::Skipped,
                skip_reason: Some(
                    crate::protocol::v2::ProjectValidationSkipReason::NoApplicableProvider,
                ),
                changed_file_count: Some(2),
                exit_code: None,
                output: "automatic validation skipped".to_string(),
                output_truncated: false,
                duration_ms: 0,
            },
        })
    );
}

#[test]
fn represents_rollback_zero_some_and_all_explicitly() {
    for count in [0, 2, u32::MAX] {
        let changes = project(
            u64::from(count).saturating_add(1),
            EventMsg::ThreadRolledBack(ThreadRolledBackEvent { num_turns: count }),
        )
        .expect("rollback projection");
        assert_eq!(
            changes.mutation(),
            Some(&ThreadHistoryProjectionMutation::RemoveLatestTurns { count })
        );
    }
}

#[test]
fn accepts_ordinal_gaps_and_rejects_missing_or_overflowing_ordinals() {
    let first = project(2, ignored_event()).expect("first ordinal");
    let later = project(10, ignored_event()).expect("gapped ordinal");
    let missing = project_rollout_line(&rollout_line(None, ignored_event()));
    let overflow = project_rollout_line(&rollout_line(Some(u64::MAX), ignored_event()));

    assert_eq!(first.ordinal(), 2);
    assert_eq!(later.ordinal(), 10);
    assert!(first.is_empty());
    assert!(later.is_empty());
    assert_eq!(missing, Err(ThreadHistoryProjectionError::MissingOrdinal));
    assert_eq!(
        overflow,
        Err(ThreadHistoryProjectionError::OrdinalOverflow(u64::MAX))
    );
}

fn project(
    ordinal: u64,
    event: EventMsg,
) -> Result<ThreadHistoryProjectionChangeSet, ThreadHistoryProjectionError> {
    project_rollout_line(&rollout_line(Some(ordinal), event))
}

fn completed_item(
    ordinal: u64,
    item: TurnItem,
) -> Result<ThreadHistoryProjectionChangeSet, ThreadHistoryProjectionError> {
    project(
        ordinal,
        EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: ThreadId::default(),
            turn_id: "turn-1".to_string(),
            item,
            completed_at_ms: 123,
        }),
    )
}

fn rollout_line(ordinal: Option<u64>, event: EventMsg) -> RolloutLine {
    RolloutLine {
        timestamp: "2026-07-12T00:00:00.000Z".to_string(),
        ordinal,
        item: RolloutItem::EventMsg(event),
    }
}

fn ignored_event() -> EventMsg {
    EventMsg::Warning(WarningEvent {
        message: "ignored".to_string(),
    })
}
