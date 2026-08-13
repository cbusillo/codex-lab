use codex_exec::ProjectValidationCompletedEvent;
use codex_exec::ProjectValidationStatus;
use codex_exec::ThreadEvent;

#[test]
fn project_validation_event_omits_absent_optional_fields() {
    let value = serde_json::to_value(ThreadEvent::ProjectValidationCompleted(
        ProjectValidationCompletedEvent {
            turn_id: "turn-1".to_string(),
            item_id: None,
            command: vec!["just".to_string(), "test".to_string()],
            command_truncated: false,
            cwd: None,
            status: ProjectValidationStatus::ConfigurationError,
            skip_reason: None,
            changed_file_count: None,
            exit_code: None,
            output: "invalid command".to_string(),
            output_truncated: false,
            duration_ms: 0,
        },
    ))
    .expect("event should serialize");

    assert!(value.get("item_id").is_none());
    assert!(value.get("cwd").is_none());
    assert!(value.get("exit_code").is_none());
}

#[test]
fn project_validation_event_preserves_item_id() {
    let value = serde_json::to_value(ThreadEvent::ProjectValidationCompleted(
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
    ))
    .expect("event should serialize");

    assert_eq!(
        value.get("item_id"),
        Some(&serde_json::Value::String("validation-item".to_string()))
    );
}
