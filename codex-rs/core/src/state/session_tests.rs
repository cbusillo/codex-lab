use super::*;
use crate::review_persistence::ReviewPersistenceContext;
use crate::session::tests::make_session_configuration_for_tests;
use crate::state::AutoCompactWindowSnapshot;
use codex_protocol::protocol::CreditsSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::ReviewPersistence;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::SpendControlLimitSnapshot;
use pretty_assertions::assert_eq;

#[test]
fn background_auto_review_schedules_only_changed_dirty_fingerprint() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);

    let schedule = state.complete_regular_turn("turn-1", Some("sha256:new".to_string()));

    let schedule = schedule.expect("changed dirty fingerprint should schedule review");
    assert_eq!(schedule.generation, 1);
    assert_eq!(schedule.fingerprint, "sha256:new");
    assert!(state.is_current_schedule(1, "sha256:new"));
}

#[test]
fn background_auto_review_skips_when_start_fingerprint_is_pending() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());

    let schedule = state.complete_regular_turn("turn-1", Some("sha256:new".to_string()));

    assert_eq!(schedule, None);
}

#[test]
fn background_auto_review_start_update_does_not_reinsert_completed_turn() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    assert_eq!(
        state.complete_regular_turn("turn-1", Some("sha256:new".to_string())),
        None
    );
    state.update_regular_turn_start_fingerprint("turn-1", None);

    assert_eq!(
        state.complete_regular_turn("turn-1", Some("sha256:new".to_string())),
        None
    );
}

#[test]
fn background_auto_review_remove_regular_turn_clears_pending_snapshot() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.remove_regular_turn("turn-1");
    state.update_regular_turn_start_fingerprint("turn-1", None);

    assert_eq!(
        state.complete_regular_turn("turn-1", Some("sha256:new".to_string())),
        None
    );
}

#[test]
fn background_auto_review_running_review_can_be_cancelled_and_cleared() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);
    let schedule = state
        .complete_regular_turn("turn-1", Some("sha256:new".to_string()))
        .expect("changed dirty fingerprint should schedule review");
    let (persistence, _cwd) = test_background_review_persistence("running-review");
    let token = state
        .record_started(schedule.generation, &schedule.fingerprint, persistence)
        .expect("current schedule should start");

    let running = state
        .take_running_review()
        .expect("running review should be tracked");
    running.cancellation_token.cancel();
    assert!(token.running_review.cancellation_token.is_cancelled());

    assert!(state.take_running_review().is_none());
}

#[test]
fn background_auto_review_cancel_invalidates_pending_schedule() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-pending".to_string());
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);
    let schedule = state
        .complete_regular_turn("turn-1", Some("sha256:new".to_string()))
        .expect("changed dirty fingerprint should schedule review");

    assert!(state.is_current_schedule(schedule.generation, &schedule.fingerprint));
    let cancellation = state.cancel_pending_and_take_reviews();
    assert!(cancellation.pending_review.is_none());
    assert!(cancellation.running_review.is_none());

    assert!(!state.is_current_schedule(schedule.generation, &schedule.fingerprint));
    assert_eq!(
        state.complete_regular_turn("turn-pending", Some("sha256:late".to_string())),
        None
    );
}

#[test]
fn background_auto_review_control_takes_pending_by_run_id() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-pending".to_string());
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);
    let schedule = state
        .complete_regular_turn("turn-1", Some("sha256:new".to_string()))
        .expect("changed dirty fingerprint should schedule review");
    let (persistence, _cwd) = test_background_review_persistence("pending-control");
    assert!(state.record_pending(schedule.generation, &schedule.fingerprint, persistence));

    let controlled_run = state
        .take_review_by_run_id("pending-control")
        .expect("pending run should be controlled by run id");
    assert!(matches!(
        controlled_run,
        BackgroundAutoReviewControlledRun::Pending(_)
    ));
    assert!(!state.is_current_schedule(schedule.generation, &schedule.fingerprint));
    assert_eq!(
        state.complete_regular_turn("turn-pending", Some("sha256:late".to_string())),
        None
    );
}

#[test]
fn background_auto_review_control_ignores_unknown_run_id() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);
    let schedule = state
        .complete_regular_turn("turn-1", Some("sha256:new".to_string()))
        .expect("changed dirty fingerprint should schedule review");
    let (persistence, _cwd) = test_background_review_persistence("pending-control");
    assert!(state.record_pending(schedule.generation, &schedule.fingerprint, persistence));

    assert!(state.take_review_by_run_id("missing-control").is_none());
    assert!(state.is_current_schedule(schedule.generation, &schedule.fingerprint));
    assert!(
        state
            .record_started(
                schedule.generation,
                &schedule.fingerprint,
                test_background_review_persistence("pending-control").0,
            )
            .is_some()
    );
}

#[test]
fn background_auto_review_control_takes_running_by_run_id() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);
    let schedule = state
        .complete_regular_turn("turn-1", Some("sha256:new".to_string()))
        .expect("changed dirty fingerprint should schedule review");
    let (persistence, _cwd) = test_background_review_persistence("running-control");
    let start = state
        .record_started(schedule.generation, &schedule.fingerprint, persistence)
        .expect("current schedule should start");

    let controlled_run = state
        .take_review_by_run_id("running-control")
        .expect("running run should be controlled by run id");
    let BackgroundAutoReviewControlledRun::Running(running_review) = controlled_run else {
        panic!("expected running review control handle");
    };
    running_review.cancellation_token.cancel();
    assert!(start.running_review.cancellation_token.is_cancelled());
    assert!(state.take_running_review().is_none());
}

#[test]
fn background_auto_review_clear_running_review_honors_generation() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);
    let first_schedule = state
        .complete_regular_turn("turn-1", Some("sha256:new".to_string()))
        .expect("changed dirty fingerprint should schedule review");
    let (persistence, _cwd) = test_background_review_persistence("clear-running-review");
    let first_token = state
        .record_started(
            first_schedule.generation,
            &first_schedule.fingerprint,
            persistence,
        )
        .expect("current schedule should start");

    state.clear_running_review(first_schedule.generation + 1);

    state.begin_regular_turn("turn-2".to_string());
    state.update_regular_turn_start_fingerprint("turn-2", Some("sha256:new".to_string()));
    let second_schedule = state
        .complete_regular_turn("turn-2", Some("sha256:newer".to_string()))
        .expect("changed dirty fingerprint should schedule newer review");

    let (persistence, _cwd) = test_background_review_persistence("clear-running-review-newer");
    state
        .record_started(
            second_schedule.generation,
            &second_schedule.fingerprint,
            persistence,
        )
        .expect("newer schedule should start after mismatched clear");
    assert!(first_token.running_review.cancellation_token.is_cancelled());
    state.clear_running_review(second_schedule.generation);
    assert!(state.take_running_review().is_none());
}

#[test]
fn background_auto_review_duplicate_check_starts_after_review_starts() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);
    let first = state
        .complete_regular_turn("turn-1", Some("sha256:new".to_string()))
        .expect("changed dirty fingerprint should schedule review");

    state.begin_regular_turn("turn-2".to_string());
    state.update_regular_turn_start_fingerprint("turn-2", Some("sha256:old".to_string()));
    let second = state.complete_regular_turn("turn-2", Some("sha256:new".to_string()));

    let second = second.expect("abandoned schedule should not suppress same fingerprint");
    let (first_persistence, _first_cwd) = test_background_review_persistence("first-started");
    assert!(
        state
            .record_started(first.generation, &first.fingerprint, first_persistence)
            .is_none()
    );
    let (second_persistence, _second_cwd) = test_background_review_persistence("second-started");
    assert!(
        state
            .record_started(second.generation, &second.fingerprint, second_persistence)
            .is_some()
    );
}

#[test]
fn background_auto_review_skips_unchanged_dirty_fingerprint() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", Some("sha256:old".to_string()));

    let schedule = state.complete_regular_turn("turn-1", Some("sha256:old".to_string()));

    assert_eq!(schedule, None);
}

#[test]
fn background_auto_review_skips_duplicate_fingerprint() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);
    let schedule = state
        .complete_regular_turn("turn-1", Some("sha256:new".to_string()))
        .expect("changed dirty fingerprint should schedule review");
    let (persistence, _cwd) = test_background_review_persistence("duplicate-started");
    assert!(
        state
            .record_started(schedule.generation, &schedule.fingerprint, persistence)
            .is_some()
    );
    state.begin_regular_turn("turn-2".to_string());
    state.update_regular_turn_start_fingerprint("turn-2", Some("sha256:old".to_string()));

    let schedule = state.complete_regular_turn("turn-2", Some("sha256:new".to_string()));

    assert_eq!(schedule, None);
}

fn test_background_review_persistence(
    run_id: &str,
) -> (ReviewPersistenceContext, tempfile::TempDir) {
    let cwd = tempfile::TempDir::new().expect("create temp cwd");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build test runtime");
    let persistence = runtime.block_on(ReviewPersistenceContext::new(
        run_id.to_string(),
        ReviewPersistence::BackgroundAutoReview,
        ReviewTarget::UncommittedChanges,
        cwd.path(),
        Some("test-model".to_string()),
    ));
    (persistence, cwd)
}

#[test]
fn background_auto_review_mismatched_completion_preserves_other_turn_snapshot() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-2".to_string());
    state.update_regular_turn_start_fingerprint("turn-2", Some("sha256:old".to_string()));

    assert_eq!(
        state.complete_regular_turn("turn-1", Some("sha256:new".to_string())),
        None
    );
    assert!(
        state
            .complete_regular_turn("turn-2", Some("sha256:new".to_string()))
            .is_some()
    );
}

#[test]
fn background_auto_review_skips_unknown_fingerprint() {
    let mut state = BackgroundAutoReviewSchedulerState::default();
    state.begin_regular_turn("turn-1".to_string());
    state.update_regular_turn_start_fingerprint("turn-1", None);

    let schedule = state.complete_regular_turn("turn-1", Some("unknown".to_string()));

    assert_eq!(schedule, None);
}

#[tokio::test]
// Verifies connector merging deduplicates repeated IDs.
async fn merge_connector_selection_deduplicates_entries() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);
    let merged = state.merge_connector_selection([
        "calendar".to_string(),
        "calendar".to_string(),
        "drive".to_string(),
    ]);

    assert_eq!(
        merged,
        HashSet::from(["calendar".to_string(), "drive".to_string()])
    );
}

#[tokio::test]
// Verifies clearing connector selection removes all saved IDs.
async fn clear_connector_selection_removes_entries() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);
    state.merge_connector_selection(["calendar".to_string()]);

    state.clear_connector_selection();

    assert_eq!(state.get_connector_selection(), HashSet::new());
}

#[tokio::test]
async fn set_rate_limits_defaults_limit_id_to_codex_when_missing() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_rate_limits(RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 12.0,
            window_minutes: Some(60),
            resets_at: Some(100),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached_type: None,
    });

    assert_eq!(
        state
            .latest_rate_limits
            .as_ref()
            .and_then(|v| v.limit_id.clone()),
        Some("codex".to_string())
    );
}

#[tokio::test]
async fn replace_history_clears_auto_compact_window_prefill_without_advancing() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.start_next_auto_compact_window();
    state.set_auto_compact_window_estimated_prefill(/*tokens*/ 100);
    state.replace_history(Vec::new(), /*reference_context_item*/ None);

    assert_eq!(
        state.auto_compact_window_snapshot(),
        AutoCompactWindowSnapshot {
            ordinal: 2,
            prefill_input_tokens: None,
        }
    );
}

#[tokio::test]
async fn set_rate_limits_defaults_to_codex_when_limit_id_missing_after_other_bucket() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_rate_limits(RateLimitSnapshot {
        limit_id: Some("codex_other".to_string()),
        limit_name: Some("codex_other".to_string()),
        primary: Some(RateLimitWindow {
            used_percent: 20.0,
            window_minutes: Some(60),
            resets_at: Some(200),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached_type: None,
    });
    state.set_rate_limits(RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 30.0,
            window_minutes: Some(60),
            resets_at: Some(300),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached_type: None,
    });

    assert_eq!(
        state
            .latest_rate_limits
            .as_ref()
            .and_then(|v| v.limit_id.clone()),
        Some("codex".to_string())
    );
}

#[tokio::test]
async fn set_rate_limits_carries_account_metadata_from_codex_to_codex_other() {
    let session_configuration = make_session_configuration_for_tests().await;
    let mut state = SessionState::new(session_configuration);

    state.set_rate_limits(RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: Some("codex".to_string()),
        primary: Some(RateLimitWindow {
            used_percent: 10.0,
            window_minutes: Some(60),
            resets_at: Some(100),
        }),
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("50".to_string()),
        }),
        individual_limit: Some(SpendControlLimitSnapshot {
            limit: "25000".to_string(),
            used: "8000".to_string(),
            remaining_percent: 68,
            resets_at: 300,
        }),
        plan_type: Some(codex_protocol::account::PlanType::Plus),
        rate_limit_reached_type: None,
    });

    state.set_rate_limits(RateLimitSnapshot {
        limit_id: Some("codex_other".to_string()),
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 30.0,
            window_minutes: Some(120),
            resets_at: Some(200),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached_type: None,
    });

    assert_eq!(
        state.latest_rate_limits,
        Some(RateLimitSnapshot {
            limit_id: Some("codex_other".to_string()),
            limit_name: None,
            primary: Some(RateLimitWindow {
                used_percent: 30.0,
                window_minutes: Some(120),
                resets_at: Some(200),
            }),
            secondary: None,
            credits: Some(CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance: Some("50".to_string()),
            }),
            individual_limit: Some(SpendControlLimitSnapshot {
                limit: "25000".to_string(),
                used: "8000".to_string(),
                remaining_percent: 68,
                resets_at: 300,
            }),
            plan_type: Some(codex_protocol::account::PlanType::Plus),
            rate_limit_reached_type: None,
        })
    );
}
