use super::*;
use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationSkipReason;
use codex_protocol::protocol::ProjectValidationStatus;

#[test]
fn project_validation_dispositions_are_persisted_in_all_history_modes() {
    let event = EventMsg::ProjectValidationCompleted(ProjectValidationCompletedEvent {
        turn_id: "turn-1".to_string(),
        item_id: None,
        command: Vec::new(),
        command_truncated: false,
        cwd: None,
        status: ProjectValidationStatus::Skipped,
        skip_reason: Some(ProjectValidationSkipReason::NoApplicableProvider),
        changed_file_count: Some(1),
        exit_code: None,
        output: "automatic validation skipped".to_string(),
        output_truncated: false,
        duration_ms: 0,
    });

    for history_mode in [ThreadHistoryMode::Legacy, ThreadHistoryMode::Paginated] {
        assert!(should_persist_event_msg(&event, history_mode));
    }
}
