use codex_exec::ProjectValidationCompletedEvent;
use codex_exec::ProjectValidationStatus;
use codex_exec::ThreadEvent;

#[test]
fn project_validation_event_omits_absent_optional_fields() {
    let value = serde_json::to_value(ThreadEvent::ProjectValidationCompleted(
        ProjectValidationCompletedEvent {
            command: vec!["just".to_string(), "test".to_string()],
            cwd: None,
            status: ProjectValidationStatus::ConfigurationError,
            exit_code: None,
            output: "invalid command".to_string(),
            output_truncated: false,
            duration_ms: 0,
        },
    ))
    .expect("event should serialize");

    assert!(value.get("cwd").is_none());
    assert!(value.get("exit_code").is_none());
}
