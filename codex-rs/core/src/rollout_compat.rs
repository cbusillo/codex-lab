use codex_history::RolloutItem as HistoryRolloutItem;
use codex_protocol::protocol::CompactedItem;
use codex_protocol::protocol::RolloutItem as ProtocolRolloutItem;

pub(crate) fn protocol_rollout_item(item: &HistoryRolloutItem) -> Option<ProtocolRolloutItem> {
    match item {
        HistoryRolloutItem::SessionMeta(item) => {
            Some(ProtocolRolloutItem::SessionMeta(item.clone()))
        }
        HistoryRolloutItem::ResponseItem(item) => {
            Some(ProtocolRolloutItem::ResponseItem(item.item.clone()))
        }
        HistoryRolloutItem::InterAgentCommunication(item) => {
            Some(ProtocolRolloutItem::InterAgentCommunication(item.clone()))
        }
        HistoryRolloutItem::InterAgentCommunicationMetadata { trigger_turn } => {
            Some(ProtocolRolloutItem::InterAgentCommunicationMetadata {
                trigger_turn: *trigger_turn,
            })
        }
        HistoryRolloutItem::Compacted(item) => {
            Some(ProtocolRolloutItem::Compacted(CompactedItem {
                message: item.message.clone(),
                replacement_history: item
                    .replacement_history
                    .as_ref()
                    .map(|items| items.iter().map(|item| item.item.clone()).collect()),
                window_number: item.window_number,
                first_window_id: item.first_window_id.clone(),
                previous_window_id: item.previous_window_id.clone(),
                window_id: item.window_id.clone(),
            }))
        }
        HistoryRolloutItem::TurnContext(item) => {
            Some(ProtocolRolloutItem::TurnContext(item.clone()))
        }
        HistoryRolloutItem::WorldState(item) => Some(ProtocolRolloutItem::WorldState(item.clone())),
        HistoryRolloutItem::SecurityRiskScore(_) => None,
        HistoryRolloutItem::EventMsg(item) => Some(ProtocolRolloutItem::EventMsg(item.clone())),
    }
}

pub(crate) fn protocol_rollout_items(items: &[HistoryRolloutItem]) -> Vec<ProtocolRolloutItem> {
    items.iter().filter_map(protocol_rollout_item).collect()
}
