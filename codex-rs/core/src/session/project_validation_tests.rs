use super::*;

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
