use std::borrow::Cow;

use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::InterAgentCommunication;
use uuid::Uuid;

pub(super) fn prepare_conversation_items_for_history(
    items: &[ResponseItem],
) -> Cow<'_, [ResponseItem]> {
    assign_missing_response_item_ids(Cow::Borrowed(items))
}

pub(super) fn assign_missing_response_item_ids(
    items: Cow<'_, [ResponseItem]>,
) -> Cow<'_, [ResponseItem]> {
    if items.iter().all(|item| {
        item.id().is_some_and(|id| !id.is_empty()) || response_item_id_prefix(item).is_none()
    }) {
        return items;
    }

    let mut items = items;
    for item in items.to_mut() {
        assign_missing_response_item_id(item);
    }
    items
}

pub(super) fn assign_missing_response_item_id(item: &mut ResponseItem) {
    if item.id().is_some_and(|id| !id.is_empty()) {
        return;
    }
    let Some(prefix) = response_item_id_prefix(item) else {
        return;
    };
    item.set_id(format!("{prefix}_{}", Uuid::now_v7()));
}

fn response_item_id_prefix(item: &ResponseItem) -> Option<&'static str> {
    match item {
        ResponseItem::Message { role, content, .. }
            if role == "assistant"
                && InterAgentCommunication::from_message_content(content)
                    .is_some_and(|communication| communication.encrypted_content.is_some()) =>
        {
            Some("amsg")
        }
        ResponseItem::Message { .. } => Some("msg"),
        ResponseItem::AgentMessage { .. } => Some("amsg"),
        ResponseItem::Reasoning { .. } => Some("rs"),
        ResponseItem::LocalShellCall { .. } => Some("lsh"),
        ResponseItem::FunctionCall { .. } => Some("fc"),
        ResponseItem::ToolSearchCall { .. } => Some("tsc"),
        ResponseItem::FunctionCallOutput { .. } => Some("fco"),
        ResponseItem::CustomToolCall { .. } => Some("ctc"),
        ResponseItem::CustomToolCallOutput { .. } => Some("ctco"),
        ResponseItem::ToolSearchOutput { .. } => Some("tso"),
        ResponseItem::WebSearchCall { .. } => Some("ws"),
        ResponseItem::ImageGenerationCall { .. } => Some("ig"),
        ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. } => Some("cmp"),
        ResponseItem::CompactionTrigger {} | ResponseItem::Other => None,
    }
}
