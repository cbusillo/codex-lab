use super::*;

#[test]
fn notification_omits_absent_optional_fields() {
    let value = serde_json::to_value(ProjectValidationCompletedNotification {
        thread_id: "thread-1".to_string(),
        turn_id: "turn-1".to_string(),
        command: vec!["just".to_string(), "test".to_string()],
        cwd: None,
        status: ProjectValidationStatus::ConfigurationError,
        exit_code: None,
        output: "invalid command".to_string(),
        output_truncated: false,
        duration_ms: 0,
    })
    .expect("notification should serialize");

    assert!(value.get("cwd").is_none());
    assert!(value.get("exitCode").is_none());
}
