use codex_protocol::ThreadId;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::Settings;
use codex_protocol::items::PlanItem;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::BackgroundAutoReviewStatus;
use codex_protocol::protocol::BackgroundAutoReviewStatusEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExitedReviewModeEvent;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::ThreadSettingsAppliedEvent;
use codex_protocol::protocol::ThreadSettingsSnapshot;
use codex_protocol::protocol::UserMessageEvent;
use codex_protocol::user_input::UserInput;
use pretty_assertions::assert_eq;

use super::persisted_rollout_items;
use super::should_persist_event_msg;

fn completed_item(item: TurnItem) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
        thread_id: ThreadId::default(),
        turn_id: "turn-1".to_string(),
        item,
        completed_at_ms: 0,
    }))
}

#[test]
fn history_mode_selects_canonical_or_legacy_user_message() {
    let response_item = RolloutItem::ResponseItem(ResponseItem::Message {
        id: Some("msg-response".to_string()),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "hello".to_string(),
        }],
        phase: None,
    });
    let completed_user_message = completed_item(TurnItem::UserMessage(UserMessageItem::new(&[
        UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        },
    ])));
    let legacy_user_message = RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
        message: "hello".to_string(),
        ..Default::default()
    }));
    let items = vec![
        response_item.clone(),
        completed_user_message.clone(),
        legacy_user_message.clone(),
    ];

    let legacy = persisted_rollout_items(&items, ThreadHistoryMode::Legacy);
    assert_eq!(
        serde_json::to_value(legacy).expect("serialize legacy items"),
        serde_json::to_value([response_item.clone(), legacy_user_message])
            .expect("serialize expected legacy items")
    );

    let paginated = persisted_rollout_items(&items, ThreadHistoryMode::Paginated);
    assert_eq!(
        serde_json::to_value(paginated).expect("serialize paginated items"),
        serde_json::to_value([response_item, completed_user_message])
            .expect("serialize expected paginated items")
    );
}

#[test]
fn review_lifecycle_stays_distinct_from_background_review_state() {
    let entered = EventMsg::EnteredReviewMode(ReviewRequest {
        target: ReviewTarget::UncommittedChanges,
        user_facing_hint: Some("Review requested.".to_string()),
    });
    let exited = EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
        review_output: None,
    });
    let background = EventMsg::BackgroundAutoReviewStatus(BackgroundAutoReviewStatusEvent {
        run_id: "review-1".to_string(),
        status: BackgroundAutoReviewStatus::Completed,
        review_target: ReviewTarget::UncommittedChanges,
        error_summary: None,
    });

    for history_mode in [ThreadHistoryMode::Legacy, ThreadHistoryMode::Paginated] {
        assert!(should_persist_event_msg(&entered, history_mode));
        assert!(should_persist_event_msg(&exited, history_mode));
        assert!(!should_persist_event_msg(&background, history_mode));
    }
}

#[test]
fn legacy_special_and_always_persisted_events_keep_their_policy() {
    let plan = EventMsg::ItemCompleted(ItemCompletedEvent {
        thread_id: ThreadId::default(),
        turn_id: "turn-1".to_string(),
        item: TurnItem::Plan(PlanItem {
            id: "plan-1".to_string(),
            text: "Inspect the rollout".to_string(),
        }),
        completed_at_ms: 0,
    });
    let rollback = EventMsg::ThreadRolledBack(ThreadRolledBackEvent { num_turns: 1 });

    for history_mode in [ThreadHistoryMode::Legacy, ThreadHistoryMode::Paginated] {
        assert!(should_persist_event_msg(&plan, history_mode));
        assert!(should_persist_event_msg(&rollback, history_mode));
    }
}

#[test]
fn project_validation_dispositions_are_persisted_in_all_history_modes() {
    let event = EventMsg::ProjectValidationCompleted(ProjectValidationCompletedEvent {
        turn_id: "turn-1".to_string(),
        command: Vec::new(),
        command_truncated: false,
        cwd: None,
        status: ProjectValidationStatus::Skipped,
        skip_reason: Some(
            codex_protocol::protocol::ProjectValidationSkipReason::NoApplicableProvider,
        ),
        changed_file_count: Some(1),
        exit_code: None,
        output: "automatic validation skipped".to_string(),
        output_truncated: false,
        duration_ms: 0,
    });

    for history_mode in [ThreadHistoryMode::Legacy, ThreadHistoryMode::Paginated] {
        assert!(should_persist_event_msg(&event, history_mode));
    }
}

#[test]
fn thread_settings_applied_is_persisted_in_all_history_modes() {
    let event = EventMsg::ThreadSettingsApplied(ThreadSettingsAppliedEvent {
        thread_settings: ThreadSettingsSnapshot {
            model: "gpt-5.2-codex".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::workspace_write(),
            active_permission_profile: None,
            cwd: std::env::current_dir()
                .expect("current directory")
                .try_into()
                .expect("absolute settings cwd"),
            reasoning_effort: Some(ReasoningEffort::High),
            reasoning_summary: Some(ReasoningSummary::Auto),
            personality: None,
            collaboration_mode: CollaborationMode {
                mode: ModeKind::Default,
                settings: Settings {
                    model: "gpt-5.2-codex".to_string(),
                    reasoning_effort: Some(ReasoningEffort::High),
                    developer_instructions: None,
                },
            },
        },
    });

    for history_mode in [ThreadHistoryMode::Legacy, ThreadHistoryMode::Paginated] {
        assert!(should_persist_event_msg(&event, history_mode));
        assert_eq!(
            persisted_rollout_items(&[RolloutItem::EventMsg(event.clone())], history_mode,).len(),
            1
        );
    }
}
