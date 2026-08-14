use codex_protocol::ThreadId;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationSkipReason;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::user_input::UserInput;
use codex_rollout::CompactedItem;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;
use pretty_assertions::assert_eq;

use super::*;
use crate::protocol::v2::ProjectValidationSkipReason as V2ProjectValidationSkipReason;
use crate::protocol::v2::ProjectValidationStatus as V2ProjectValidationStatus;
use crate::protocol::v2::ThreadItem;
use crate::protocol::v2::TurnError;

#[test]
fn projects_turn_lifecycle_without_prior_builder_state() {
    let started = project(RolloutItem::EventMsg(EventMsg::TurnStarted(
        TurnStartedEvent {
            turn_id: "turn-1".to_string(),
            trace_id: None,
            started_at: Some(10),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        },
    )));
    let completed = project(RolloutItem::EventMsg(EventMsg::TurnComplete(
        TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
            error: None,
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
            time_to_first_token_ms: None,
        },
    )));

    assert_eq!(started.changed_turns.len(), 1);
    assert_eq!(started.changed_turns[0].turn_id, "turn-1");
    assert_eq!(started.changed_turns[0].status, TurnStatus::InProgress);
    assert_eq!(started.changed_turns[0].started_at, Some(10));
    assert_eq!(
        completed,
        ThreadHistoryChangeSet {
            changed_turns: vec![ThreadHistoryTurnChange {
                turn_id: "turn-1".to_string(),
                status: TurnStatus::Completed,
                error: None,
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
            }],
            ..Default::default()
        }
    );
}

#[test]
fn projects_failed_turn_completion_as_snapshot() {
    let error = ErrorEvent {
        message: "request failed".to_string(),
        codex_error_info: None,
    };

    let changes = project(RolloutItem::EventMsg(EventMsg::TurnComplete(
        TurnCompleteEvent {
            turn_id: "turn-1".to_string(),
            last_agent_message: None,
            error: Some(error),
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
            time_to_first_token_ms: None,
        },
    )));

    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_turns: vec![ThreadHistoryTurnChange {
                turn_id: "turn-1".to_string(),
                status: TurnStatus::Failed,
                error: Some(TurnError {
                    message: "request failed".to_string(),
                    codex_error_info: None,
                    additional_details: None,
                }),
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
            }],
            ..Default::default()
        }
    );
}

#[test]
fn projects_completed_canonical_turn_items() {
    let thread_id = ThreadId::default();
    let user_item = TurnItem::UserMessage(UserMessageItem {
        id: "user-1".to_string(),
        client_id: None,
        content: vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }],
    });
    let agent_item = TurnItem::AgentMessage(AgentMessageItem {
        id: "agent-1".to_string(),
        content: vec![AgentMessageContent::Text {
            text: "done".to_string(),
        }],
        phase: None,
        memory_citation: None,
    });

    let user_changes = project(item_completed(thread_id, "turn-1", user_item.clone()));
    let agent_changes = project(item_completed(thread_id, "turn-1", agent_item.clone()));

    assert_eq!(
        user_changes.changed_items,
        vec![ThreadHistoryItemChange {
            turn_id: "turn-1".to_string(),
            item: ThreadItem::from(user_item),
            started_at_ms: Some(100),
            completed_at_ms: Some(123),
        }]
    );
    assert_eq!(
        agent_changes.changed_items,
        vec![ThreadHistoryItemChange {
            turn_id: "turn-1".to_string(),
            item: ThreadItem::from(agent_item),
            started_at_ms: Some(100),
            completed_at_ms: Some(123),
        }]
    );
}

#[test]
fn projects_distinct_project_validation_attempts_by_rollout_ordinal() {
    let event = ProjectValidationCompletedEvent {
        turn_id: "turn-1".to_string(),
        item_id: None,
        command: vec!["cargo".to_string(), "check".to_string()],
        command_truncated: false,
        cwd: None,
        status: ProjectValidationStatus::Skipped,
        skip_reason: Some(ProjectValidationSkipReason::UnchangedFingerprint),
        changed_file_count: Some(2),
        exit_code: None,
        output: "automatic validation skipped".to_string(),
        output_truncated: false,
        duration_ms: 0,
    };

    let first = project_with_ordinal(
        RolloutItem::EventMsg(EventMsg::ProjectValidationCompleted(event.clone())),
        Some(7),
    );
    let second = project_with_ordinal(
        RolloutItem::EventMsg(EventMsg::ProjectValidationCompleted(event)),
        Some(8),
    );

    assert_eq!(first.changed_items.len(), 1);
    assert_eq!(second.changed_items.len(), 1);
    assert_eq!(
        first.changed_items[0],
        ThreadHistoryItemChange {
            turn_id: "turn-1".to_string(),
            item: ThreadItem::ProjectValidation {
                id: "project-validation-7".to_string(),
                command: vec!["cargo".to_string(), "check".to_string()],
                command_truncated: false,
                cwd: None,
                status: V2ProjectValidationStatus::Skipped,
                skip_reason: Some(V2ProjectValidationSkipReason::UnchangedFingerprint),
                changed_file_count: Some(2),
                exit_code: None,
                output: "automatic validation skipped".to_string(),
                output_truncated: false,
                duration_ms: 0,
            },
            started_at_ms: None,
            completed_at_ms: None,
        }
    );
    assert_eq!(second.changed_items[0].item.id(), "project-validation-8");
}

#[test]
fn project_validation_uses_persisted_item_id() {
    let changes = project_with_ordinal(
        RolloutItem::EventMsg(EventMsg::ProjectValidationCompleted(
            ProjectValidationCompletedEvent {
                turn_id: "turn-1".to_string(),
                item_id: Some("validation-item".to_string()),
                command: Vec::new(),
                command_truncated: false,
                cwd: None,
                status: ProjectValidationStatus::Passed,
                skip_reason: None,
                changed_file_count: Some(1),
                exit_code: Some(0),
                output: "ok".to_string(),
                output_truncated: false,
                duration_ms: 12,
            },
        )),
        Some(7),
    );

    assert_eq!(changes.changed_items[0].item.id(), "validation-item");
}

#[test]
fn project_validation_item_id_does_not_require_rollout_ordinal() {
    let changes = project_with_ordinal(
        RolloutItem::EventMsg(EventMsg::ProjectValidationCompleted(
            ProjectValidationCompletedEvent {
                turn_id: "turn-1".to_string(),
                item_id: Some("validation-item".to_string()),
                command: Vec::new(),
                command_truncated: false,
                cwd: None,
                status: ProjectValidationStatus::Passed,
                skip_reason: None,
                changed_file_count: Some(1),
                exit_code: Some(0),
                output: "ok".to_string(),
                output_truncated: false,
                duration_ms: 12,
            },
        )),
        /*ordinal*/ None,
    );

    assert_eq!(changes.changed_items[0].item.id(), "validation-item");
}

#[test]
fn ignores_project_validation_without_rollout_ordinal() {
    let changes = project_with_ordinal(
        RolloutItem::EventMsg(EventMsg::ProjectValidationCompleted(
            ProjectValidationCompletedEvent {
                turn_id: "turn-1".to_string(),
                item_id: None,
                command: Vec::new(),
                command_truncated: false,
                cwd: None,
                status: ProjectValidationStatus::Cancelled,
                skip_reason: None,
                changed_file_count: None,
                exit_code: None,
                output: "automatic validation cancelled".to_string(),
                output_truncated: false,
                duration_ms: 0,
            },
        )),
        /*ordinal*/ None,
    );

    assert!(changes.is_empty());
}

#[test]
fn projects_optional_completed_item_lifecycle_timestamps() {
    let item = TurnItem::UserMessage(UserMessageItem {
        id: "user-1".to_string(),
        client_id: None,
        content: Vec::new(),
    });

    for (started_at_ms, completed_at_ms, expected_completed_at_ms) in
        [(None, 123, Some(123)), (Some(100), 0, None)]
    {
        let changes = project(RolloutItem::EventMsg(EventMsg::ItemCompleted(
            ItemCompletedEvent {
                thread_id: ThreadId::default(),
                turn_id: "turn-1".to_string(),
                item: item.clone(),
                started_at_ms,
                completed_at_ms,
            },
        )));

        assert_eq!(
            changes,
            ThreadHistoryChangeSet {
                changed_items: vec![ThreadHistoryItemChange {
                    turn_id: "turn-1".to_string(),
                    item: ThreadItem::from(item.clone()),
                    started_at_ms,
                    completed_at_ms: expected_completed_at_ms,
                }],
                ..Default::default()
            }
        );
    }
}

#[test]
fn ignores_legacy_abort_without_turn_id_and_context_only_records() {
    let aborted = project(RolloutItem::EventMsg(EventMsg::TurnAborted(
        TurnAbortedEvent {
            turn_id: None,
            reason: TurnAbortReason::Interrupted,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        },
    )));
    let compacted = project(RolloutItem::Compacted(CompactedItem {
        message: String::new(),
        replacement_history: None,
        window_number: None,
        first_window_id: None,
        previous_window_id: None,
        window_id: None,
    }));

    assert!(aborted.is_empty());
    assert!(compacted.is_empty());
}

#[test]
fn projects_identified_turn_aborts() {
    let changes = project(RolloutItem::EventMsg(EventMsg::TurnAborted(
        TurnAbortedEvent {
            turn_id: Some("turn-1".to_string()),
            reason: TurnAbortReason::Interrupted,
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
        },
    )));

    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_turns: vec![ThreadHistoryTurnChange {
                turn_id: "turn-1".to_string(),
                status: TurnStatus::Interrupted,
                error: None,
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
            }],
            ..Default::default()
        }
    );
}

fn project(item: RolloutItem) -> ThreadHistoryChangeSet {
    project_with_ordinal(item, Some(7))
}

fn project_with_ordinal(item: RolloutItem, ordinal: Option<u64>) -> ThreadHistoryChangeSet {
    project_rollout_line(&RolloutLine {
        timestamp: "2026-07-09T00:00:00.000Z".to_string(),
        ordinal,
        item,
    })
}

fn item_completed(thread_id: ThreadId, turn_id: &str, item: TurnItem) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
        thread_id,
        turn_id: turn_id.to_string(),
        item,
        started_at_ms: Some(100),
        completed_at_ms: 123,
    }))
}
