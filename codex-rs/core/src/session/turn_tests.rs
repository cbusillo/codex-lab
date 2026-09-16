use super::*;
use codex_extension_api::ExtensionData;
use codex_extension_api::TurnItemContributor;
use codex_protocol::ResponseItemId;
use codex_protocol::items::AgentMessageContent;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tracing_subscriber::prelude::*;

struct RewriteAgentMessageContributor;

impl TurnItemContributor for RewriteAgentMessageContributor {
    fn contribute<'a>(
        &'a self,
        _thread_store: &'a ExtensionData,
        _turn_store: &'a ExtensionData,
        item: &'a mut TurnItem,
    ) -> codex_extension_api::ExtensionFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let TurnItem::AgentMessage(agent_message) = item {
                agent_message.content = vec![AgentMessageContent::Text {
                    text: "plan contributed assistant text".to_string(),
                }];
            }
            Ok(())
        })
    }
}

fn assistant_output_text(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some(ResponseItemId::with_suffix("msg", "1")),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn post_sampling_token_estimate_is_disabled_by_always_on_sinks() {
    let feedback = codex_feedback::CodexFeedback::new();
    let subscriber = tracing_subscriber::registry()
        .with(feedback.logger_layer())
        .with(tracing_subscriber::fmt::layer().with_filter(codex_state::log_db::default_filter()));

    tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        assert!(!tracing::event_enabled!(
            target: POST_SAMPLING_TOKEN_ESTIMATE_TARGET,
            tracing::Level::TRACE,
            turn_id,
            estimated_token_count,
            message
        ));
    });
}

#[tokio::test]
async fn plan_mode_uses_contributed_turn_item_for_last_agent_message() {
    let (mut session, turn_context) = crate::session::tests::make_session_and_context().await;
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::new();
    builder.turn_item_contributor(Arc::new(RewriteAgentMessageContributor));
    session.services.extensions = Arc::new(builder.build());
    let turn_store = ExtensionData::new(turn_context.sub_id.clone());
    let mut state = PlanModeStreamState::new(&turn_context.sub_id);
    let mut last_agent_message = None;
    let item = assistant_output_text("original assistant text");

    let step_context = StepContext::for_test(Arc::new(turn_context));
    let handled = handle_assistant_item_done_in_plan_mode(
        &session,
        &step_context,
        &turn_store,
        &item,
        &mut state,
        /*previously_active_item*/ None,
        &mut last_agent_message,
    )
    .await;

    assert!(handled);
    assert_eq!(
        last_agent_message.as_deref(),
        Some("plan contributed assistant text")
    );
}

#[test]
fn realtime_user_verification_notice_excludes_request_payload() {
    let event = EventMsg::ElicitationRequest(codex_protocol::approvals::ElicitationRequestEvent {
        turn_id: None,
        server_name: "private-server-name".to_string(),
        id: codex_protocol::mcp::RequestId::String("private-request-id".to_string()),
        request: codex_protocol::approvals::ElicitationRequest::UserVerification {
            title: "private-title".to_string(),
            description: "private-description".to_string(),
            challenge: "private-challenge".to_string(),
        },
    });
    assert_eq!(
        realtime_text_for_event(&event),
        Some((
            "<user_verification_notice>User verification is required. Please respond in the app.</user_verification_notice>".to_string(),
            None,
        )),
    );
}

#[test]
fn realtime_interactive_request_notices_exclude_request_payloads() {
    const PRIVATE: &str = "private-interactive-request-detail";
    let cases = [
        (
            EventMsg::ExecApprovalRequest(codex_protocol::approvals::ExecApprovalRequestEvent {
                kind: codex_protocol::approvals::ExecApprovalKind::Command,
                call_id: "exec-call".to_string(),
                plugin_id: None,
                script_path: None,
                approval_id: None,
                turn_id: "turn".to_string(),
                environment_id: None,
                started_at_ms: 0,
                command: vec![PRIVATE.to_string()],
                cwd: codex_utils_path_uri::LegacyAppPathString::from_string(PRIVATE),
                reason: None,
                network_approval_context: None,
                proposed_execpolicy_amendment: None,
                proposed_network_policy_amendments: None,
                additional_permissions: None,
                available_decisions: None,
                parsed_cmd: Vec::new(),
            }),
            RealtimeInteractiveRequestKind::Approval,
        ),
        (
            EventMsg::RequestPermissions(
                codex_protocol::request_permissions::RequestPermissionsEvent {
                    call_id: "permissions-call".to_string(),
                    turn_id: "turn".to_string(),
                    environment_id: None,
                    started_at_ms: 0,
                    reason: Some(PRIVATE.to_string()),
                    permissions: Default::default(),
                    cwd: None,
                },
            ),
            RealtimeInteractiveRequestKind::Approval,
        ),
        (
            EventMsg::ApplyPatchApprovalRequest(
                codex_protocol::approvals::ApplyPatchApprovalRequestEvent {
                    call_id: "patch-call".to_string(),
                    turn_id: "turn".to_string(),
                    started_at_ms: 0,
                    changes: std::collections::HashMap::from([(
                        std::path::PathBuf::from(PRIVATE),
                        codex_protocol::protocol::FileChange::Add {
                            content: PRIVATE.to_string(),
                        },
                    )]),
                    reason: None,
                    grant_root: None,
                },
            ),
            RealtimeInteractiveRequestKind::Approval,
        ),
        (
            EventMsg::RequestUserInput(codex_protocol::request_user_input::RequestUserInputEvent {
                call_id: "input-call".to_string(),
                turn_id: "turn".to_string(),
                questions: vec![
                    codex_protocol::request_user_input::RequestUserInputQuestion {
                        id: "question".to_string(),
                        header: "Input".to_string(),
                        question: PRIVATE.to_string(),
                        is_other: false,
                        is_secret: true,
                        options: None,
                    },
                ],
                is_blocking: true,
                auto_resolution_ms: None,
            }),
            RealtimeInteractiveRequestKind::Input,
        ),
        (
            EventMsg::ElicitationRequest(codex_protocol::approvals::ElicitationRequestEvent {
                turn_id: Some("turn".to_string()),
                server_name: "private-server".to_string(),
                id: codex_protocol::mcp::RequestId::String("elicitation".to_string()),
                request: codex_protocol::approvals::ElicitationRequest::Url {
                    meta: None,
                    message: PRIVATE.to_string(),
                    url: PRIVATE.to_string(),
                    elicitation_id: "elicitation".to_string(),
                },
            }),
            RealtimeInteractiveRequestKind::Input,
        ),
    ];

    for (event, kind) in cases {
        let expected = Some((RealtimeInteractiveRequestNotice::new(kind).render(), None));
        let actual = realtime_text_for_event(&event);
        assert_eq!(actual, expected);
        assert!(
            !actual
                .expect("interactive request notice")
                .0
                .contains(PRIVATE)
        );
    }
}
