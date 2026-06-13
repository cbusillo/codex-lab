use super::*;
use crate::session::tests::make_session_configuration_for_tests;
use crate::state::AutoCompactWindowSnapshot;
use codex_protocol::protocol::CreditsSnapshot;
use codex_protocol::protocol::RateLimitWindow;
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
    let token = state
        .record_started(schedule.generation, &schedule.fingerprint)
        .expect("current schedule should start");

    state.cancel_running_review();
    assert!(token.is_cancelled());
    state.clear_running_review(schedule.generation);
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
    assert!(
        state
            .record_started(first.generation, &first.fingerprint)
            .is_none()
    );
    assert!(
        state
            .record_started(second.generation, &second.fingerprint)
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
    assert!(
        state
            .record_started(schedule.generation, &schedule.fingerprint)
            .is_some()
    );
    state.begin_regular_turn("turn-2".to_string());
    state.update_regular_turn_start_fingerprint("turn-2", Some("sha256:old".to_string()));

    let schedule = state.complete_regular_turn("turn-2", Some("sha256:new".to_string()));

    assert_eq!(schedule, None);
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
