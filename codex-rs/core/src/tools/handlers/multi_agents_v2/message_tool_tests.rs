use super::agent_message_from_tool;
use super::message_tool::message_content;
use crate::agent::control::MessageDeliveryMode;
use crate::session_prefix::bounded_completion_payload;
use crate::tools::context::ToolPayload;
use crate::tools::router::ToolCall;
use codex_protocol::AgentPath;
use codex_tools::ToolName;

#[test]
fn agent_messages_are_bounded_before_delivery() {
    let message = "🙂".repeat(20_000);

    let bounded = message_content(message.clone()).expect("non-empty message should be accepted");

    assert_eq!(bounded, bounded_completion_payload(&message));
    assert_ne!(bounded, message);
}

#[test]
fn omitted_encryption_metadata_keeps_internal_message_plaintext() {
    let call = ToolCall {
        tool_name: ToolName::plain("spawn_agent"),
        call_id: "call-plaintext".to_string(),
        payload: ToolPayload::Function {
            arguments: String::new(),
        },
        encrypted_function_args: None,
    };
    let author = AgentPath::root();
    let recipient = author.join("worker").expect("valid agent path");
    let communication =
        agent_message_from_tool("inspect this repository".to_string(), &call.direct_source())
            .into_communication(author, recipient, MessageDeliveryMode::TriggerTurn);

    assert!(communication.content.contains("inspect this repository"));
    assert_eq!(communication.encrypted_content, None);
}
