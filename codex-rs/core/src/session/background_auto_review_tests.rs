use super::background_auto_review_size_limit_summary;
use pretty_assertions::assert_eq;

#[test]
fn background_auto_review_size_limit_summary_prefers_raw_diff_limit() {
    let summary = background_auto_review_size_limit_summary(
        "abcdef",
        Some("abcdef wrapped prompt"),
        /*max_bytes*/ 5,
    );

    assert_eq!(
        summary.as_deref(),
        Some("diff exceeds background review size limit: 6 bytes > 5 bytes")
    );
}

#[test]
fn background_auto_review_size_limit_summary_catches_wrapped_scope_limit() {
    let summary =
        background_auto_review_size_limit_summary("abc", Some("abcdef"), /*max_bytes*/ 5);

    assert_eq!(
        summary.as_deref(),
        Some(
            "auto review scope exceeds configured background review size limit: scope is 6 \
             bytes, diff is 3 bytes (limit 5 bytes)"
        )
    );
}

#[test]
fn background_auto_review_size_limit_summary_allows_in_budget_scope() {
    let summary =
        background_auto_review_size_limit_summary("abc", Some("abcde"), /*max_bytes*/ 5);

    assert_eq!(summary, None);
}
