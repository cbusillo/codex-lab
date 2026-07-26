use codex_protocol::AgentPath;
use codex_protocol::protocol::AgentStatus;
use codex_utils_output_truncation::approx_token_count;
use pretty_assertions::assert_eq;

use super::COMPLETION_MESSAGE_MAX_TOKENS;
use super::ERROR_NEXT_ACTION;
use super::bounded_completion_payload;
use super::bounded_status;
use super::format_inter_agent_completion_message;
use super::format_subagent_notification_message;

fn completion_message(status: AgentStatus) -> String {
    format_inter_agent_completion_message(
        AgentPath::root(),
        AgentPath::try_from("/root/worker").expect("valid agent path"),
        &status,
    )
    .expect("final status should produce a completion message")
}

#[test]
fn error_completion_message_stays_below_manual_review_threshold() {
    let message = completion_message(AgentStatus::Errored("stream disconnected ".repeat(1_000)));

    assert!(approx_token_count(&message) < COMPLETION_MESSAGE_MAX_TOKENS);
    assert!(message.contains(ERROR_NEXT_ACTION));
}

#[test]
fn completed_completion_message_stays_below_manual_review_threshold() {
    let message = completion_message(AgentStatus::Completed(Some(
        "here is my very long final answer ".repeat(1_000),
    )));

    assert!(
        approx_token_count(&message) < COMPLETION_MESSAGE_MAX_TOKENS,
        "{} tokens",
        approx_token_count(&message)
    );
    assert!(message.starts_with("Message Type: FINAL_ANSWER"));
    assert!(message.contains("here is my very long final answer"));
}

#[test]
fn short_completion_message_is_left_intact() {
    assert_eq!(
        completion_message(AgentStatus::Completed(Some("done".to_string()))),
        "Message Type: FINAL_ANSWER\nTask name: /root\nSender: /root/worker\nPayload:\ndone"
    );
}

#[test]
fn subagent_notification_bounds_completed_and_errored_payloads() {
    for status in [
        AgentStatus::Completed(Some("final answer ".repeat(2_000))),
        AgentStatus::Errored("boom ".repeat(2_000)),
    ] {
        let message = format_subagent_notification_message("/root/worker", &status);
        assert!(
            approx_token_count(&message) < COMPLETION_MESSAGE_MAX_TOKENS,
            "{} tokens for {status:?}",
            approx_token_count(&message)
        );
    }
}

#[test]
fn bounded_status_preserves_statuses_without_agent_authored_text() {
    for status in [
        AgentStatus::Completed(None),
        AgentStatus::Shutdown,
        AgentStatus::NotFound,
        AgentStatus::PendingInit,
        AgentStatus::Running,
        AgentStatus::Interrupted,
    ] {
        assert_eq!(bounded_status(&status), status);
    }
}

#[test]
fn bounded_completion_payload_keeps_multibyte_text_valid() {
    let payload = bounded_completion_payload(&"🙂".repeat(20_000));

    assert!(approx_token_count(&payload) <= COMPLETION_MESSAGE_MAX_TOKENS);
    assert!(payload.starts_with('🙂'));
}
