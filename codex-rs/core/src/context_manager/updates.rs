use crate::context::ContextualUserFragment;
use crate::context::world_state::MultiAgentUsageHintState;
use crate::context::world_state::WorldStateSection;
use crate::context::world_state::WorldStateSnapshot;
use codex_history::CodexHarnessMetadata;
use codex_history::ContextFragmentKind;
use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageGroup {
    Standalone,
    Mergeable,
}

pub(crate) fn build_developer_update_item(text_sections: Vec<String>) -> Option<ResponseItem> {
    build_text_message("developer", text_sections)
}

pub(crate) fn build_contextual_user_message(text_sections: Vec<String>) -> Option<ResponseItem> {
    build_text_message("user", text_sections)
}

pub(crate) fn merge_contextual_fragments(
    fragments: Vec<Box<dyn ContextualUserFragment>>,
) -> Vec<ResponseItem> {
    let mut messages: Vec<(&str, MessageGroup, Vec<String>)> = Vec::with_capacity(fragments.len());
    for fragment in fragments {
        let role = fragment.role();
        let group = if fragment.requires_separate_message() {
            MessageGroup::Standalone
        } else {
            MessageGroup::Mergeable
        };
        let text = fragment.render();
        match messages.last_mut() {
            Some((previous_role, previous_group, text_sections))
                if *previous_role == role
                    && *previous_group == MessageGroup::Mergeable
                    && group == MessageGroup::Mergeable =>
            {
                text_sections.push(text);
            }
            _ => messages.push((role, group, vec![text])),
        }
    }
    messages
        .into_iter()
        .filter_map(|(role, _, text_sections)| build_text_message(role, text_sections))
        .collect()
}

pub(crate) fn annotate_multi_agent_usage_hint(
    items: &mut [ResponseItemEnvelope],
    world_state_snapshot: &WorldStateSnapshot,
) {
    let Some(identity) =
        world_state_snapshot.fragment_identity(MultiAgentUsageHintState::ID, "developer")
    else {
        return;
    };

    let matching_indices = items
        .windows(2)
        .enumerate()
        .filter_map(|(index, pair)| {
            let is_usage_hint = matches!(
                &pair[0].item,
                ResponseItem::Message { role, content, .. }
                    if role == "developer"
                        && matches!(
                            content.as_slice(),
                            [ContentItem::InputText { text }] if identity.matches(role, text)
                        )
            );
            let is_multi_agent_mode = matches!(
                &pair[1].item,
                ResponseItem::Message { role, content, .. }
                    if role == "developer"
                        && content.iter().any(|item| matches!(
                            item,
                            ContentItem::InputText { text }
                                if text.contains(MULTI_AGENT_MODE_OPEN_TAG)
                        ))
            );
            (is_usage_hint && is_multi_agent_mode).then_some(index)
        })
        .collect::<Vec<_>>();

    for index in matching_indices {
        items[index].metadata = Some(CodexHarnessMetadata {
            context_fragment: Some(ContextFragmentKind::MultiAgentUsageHint),
            ..items[index].metadata.take().unwrap_or_default()
        });
    }
}

fn build_text_message(role: &str, text_sections: Vec<String>) -> Option<ResponseItem> {
    if text_sections.is_empty() {
        return None;
    }

    let content = text_sections
        .into_iter()
        .map(|text| ContentItem::InputText { text })
        .collect();

    Some(ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content,
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    })
}
