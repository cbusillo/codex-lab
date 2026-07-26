use super::*;

#[test]
fn truncates_command_within_hard_byte_cap() {
    let command = vec!["x".repeat(PROJECT_VALIDATION_COMMAND_MAX_BYTES * 2)];
    let (truncated, did_truncate) = truncate_command(command);
    let command_bytes = truncated.iter().fold(0usize, |total, argument| {
        total.saturating_add(argument.len() + 1)
    });

    assert!(did_truncate);
    assert!(command_bytes <= PROJECT_VALIDATION_COMMAND_MAX_BYTES);
    assert_eq!(
        truncated.last().map(String::as_str),
        Some(COMMAND_TRUNCATED_MARKER)
    );
}

#[test]
fn truncates_output_within_hard_byte_cap() {
    let output = "a".repeat(PROJECT_VALIDATION_OUTPUT_MAX_BYTES * 2);
    let (truncated, did_truncate) = truncate_output(&output);

    assert!(did_truncate);
    assert!(truncated.len() <= PROJECT_VALIDATION_OUTPUT_MAX_BYTES);
    assert!(truncated.contains(OUTPUT_TRUNCATED_MARKER));
    assert!(truncated.starts_with('a'));
    assert!(truncated.ends_with('a'));
}

#[test]
fn truncates_utf8_output_on_character_boundaries() {
    let output = "🦀".repeat(PROJECT_VALIDATION_OUTPUT_MAX_BYTES);
    let (truncated, did_truncate) = truncate_output(&output);

    assert!(did_truncate);
    assert!(truncated.len() <= PROJECT_VALIDATION_OUTPUT_MAX_BYTES);
    assert!(truncated.contains(OUTPUT_TRUNCATED_MARKER));
}
