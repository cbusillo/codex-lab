use super::*;
use pretty_assertions::assert_eq;

#[test]
fn bound_environment_context_body_keeps_bodies_within_the_cap_untouched() {
    let body = "a".repeat(MAX_ENVIRONMENT_CONTEXT_BODY_BYTES);

    assert_eq!(bound_environment_context_body(body.clone()), body);
}

#[test]
fn bound_environment_context_body_caps_oversize_bodies() {
    let bounded =
        bound_environment_context_body("a".repeat(MAX_ENVIRONMENT_CONTEXT_BODY_BYTES * 4));

    assert!(bounded.len() <= MAX_ENVIRONMENT_CONTEXT_BODY_BYTES);
    assert!(bounded.ends_with(TRUNCATION_NOTICE));
}

#[test]
fn bound_environment_context_body_truncates_multibyte_text_on_a_char_boundary() {
    let bounded = bound_environment_context_body("🙂".repeat(MAX_ENVIRONMENT_CONTEXT_BODY_BYTES));

    assert!(bounded.len() <= MAX_ENVIRONMENT_CONTEXT_BODY_BYTES);
    assert!(bounded.ends_with(TRUNCATION_NOTICE));
    // Reaching this point at all proves the slice was taken on a UTF-8 boundary; assert the
    // surviving prefix is still whole emoji rather than replacement bytes.
    let prefix = bounded.strip_suffix(TRUNCATION_NOTICE).expect("notice");
    assert!(prefix.chars().all(|ch| ch == '🙂'));
}
