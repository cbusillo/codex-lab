use super::message_tool::message_content;
use crate::session_prefix::bounded_completion_payload;

#[test]
fn agent_messages_are_bounded_before_delivery() {
    let message = "🙂".repeat(20_000);

    let bounded = message_content(message.clone()).expect("non-empty message should be accepted");

    assert_eq!(bounded, bounded_completion_payload(&message));
    assert_ne!(bounded, message);
}
