use super::BACKGROUND_AUTO_REVIEW_DEBOUNCE;
use super::acquire_background_auto_review_lock;
use super::background_auto_review_duplicate_is_visible_to_thread;
use super::background_auto_review_size_limit_summary;
use codex_auto_review::AutoReviewRunState;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::ReviewCoordination;
use pretty_assertions::assert_eq;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn background_auto_review_size_limit_summary_prefers_raw_diff_limit() {
    let summary = background_auto_review_size_limit_summary(
        "abcdef",
        Some("abcdef wrapped prompt"),
        /*max_bytes*/ 5,
    );

    assert_eq!(
        summary.as_deref(),
        Some(
            "diff exceeds background review size limit: 6 bytes > 5 bytes; retry with a narrower turn or increase `auto_review.background_max_diff_bytes`"
        )
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
             bytes, diff is 3 bytes (limit 5 bytes); retry with a narrower turn or increase \
             `auto_review.background_max_diff_bytes`"
        )
    );
}

#[test]
fn background_auto_review_size_limit_summary_allows_in_budget_scope() {
    let summary =
        background_auto_review_size_limit_summary("abc", Some("abcde"), /*max_bytes*/ 5);

    assert_eq!(summary, None);
}

#[test]
fn background_auto_review_duplicate_visibility_respects_thread_ownership() {
    let codex_home = TempDir::new().expect("create temp codex home");
    let scope = TempDir::new().expect("create temp scope");
    let store = AutoReviewStore::for_scope(codex_home.path(), scope.path());

    assert!(background_auto_review_duplicate_is_visible_to_thread(
        &store,
        "legacy-without-state",
        "thread-a",
    ));

    let mut ownerless = AutoReviewRunState::new("ownerless");
    store
        .save_run_state(&ownerless)
        .expect("save ownerless run state");
    assert!(background_auto_review_duplicate_is_visible_to_thread(
        &store,
        "ownerless",
        "thread-a",
    ));

    ownerless.run_id = "same-owner".to_string();
    ownerless.owner_thread_id = Some("thread-a".to_string());
    store
        .save_run_state(&ownerless)
        .expect("save same-owner run state");
    assert!(background_auto_review_duplicate_is_visible_to_thread(
        &store,
        "same-owner",
        "thread-a",
    ));

    ownerless.run_id = "foreign-owner".to_string();
    ownerless.owner_thread_id = Some("thread-b".to_string());
    store
        .save_run_state(&ownerless)
        .expect("save foreign-owner run state");
    assert!(!background_auto_review_duplicate_is_visible_to_thread(
        &store,
        "foreign-owner",
        "thread-a",
    ));
}

#[tokio::test(start_paused = true)]
async fn background_auto_review_lock_retry_outlasts_pending_debounce_handoff() {
    let codex_home = TempDir::new().expect("create temp codex home");
    let scope = TempDir::new().expect("create temp scope");
    let coordination = ReviewCoordination::for_scope(codex_home.path(), scope.path());
    let held_guard = coordination
        .try_acquire_lock("first-pending")
        .expect("acquire first review lock")
        .expect("first review lock must be available");

    tokio::spawn(async move {
        tokio::time::sleep(BACKGROUND_AUTO_REVIEW_DEBOUNCE + Duration::from_millis(100)).await;
        drop(held_guard);
    });

    let acquired = acquire_background_auto_review_lock(&coordination, "second-pending".to_string())
        .await
        .expect("retry review lock acquisition");
    assert!(
        acquired.is_some(),
        "retry budget must outlast the pending review's debounce window"
    );
}
