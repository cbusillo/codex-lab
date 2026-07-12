use codex_protocol::ThreadId;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::USER_MESSAGE_BEGIN;
use codex_protocol::user_input::UserInput;

use super::case_insensitive_literal_regex;
use super::content_match_snippet;

#[test]
fn completed_user_message_produces_search_snippet() {
    let thread_id = ThreadId::default();
    let line = RolloutLine {
        timestamp: "2026-07-12T00:00:00Z".to_string(),
        item: RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id,
            turn_id: "turn-1".to_string(),
            item: TurnItem::UserMessage(UserMessageItem::new(&[UserInput::Text {
                text: format!("{USER_MESSAGE_BEGIN} find the paginated needle"),
                text_elements: Vec::new(),
            }])),
            completed_at_ms: 0,
        })),
    };
    let json = serde_json::to_string(&line).expect("serialize paginated rollout line");
    let search_term = case_insensitive_literal_regex("paginated needle").expect("search regex");

    assert_eq!(
        content_match_snippet(json.as_str(), &search_term).as_deref(),
        Some("find the paginated needle")
    );
}
