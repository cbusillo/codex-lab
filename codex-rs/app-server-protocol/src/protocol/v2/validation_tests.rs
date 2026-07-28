use super::*;

#[test]
fn notification_serializes_absent_output_fields_as_null() {
    let value = serde_json::to_value(ProjectValidationCompletedNotification {
        thread_id: "thread-1".to_string(),
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
    })
    .expect("notification should serialize");

    assert_eq!(value.get("itemId"), Some(&serde_json::Value::Null));
    assert_eq!(value.get("cwd"), Some(&serde_json::Value::Null));
    assert_eq!(value.get("skipReason"), Some(&serde_json::Value::Null));
    assert_eq!(
        value.get("changedFileCount"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(value.get("exitCode"), Some(&serde_json::Value::Null));
}
