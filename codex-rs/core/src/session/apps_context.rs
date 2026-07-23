use std::sync::Arc;

use arc_swap::ArcSwapOption;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::protocol::APPS_INSTRUCTIONS_CLOSE_TAG;
use codex_protocol::protocol::APPS_INSTRUCTIONS_OPEN_TAG;

use super::*;
use crate::context::AppsInstructions;
use crate::context::ContextualUserFragment;

pub(super) struct AppsContext {
    fragment: ArcSwapOption<ResponseItem>,
}

impl Default for AppsContext {
    fn default() -> Self {
        Self {
            fragment: ArcSwapOption::empty(),
        }
    }
}

impl AppsContext {
    fn update(&self, available: bool) {
        let current = self.fragment.load_full();
        match (current.is_some(), available) {
            (false, true) => self
                .fragment
                .store(Some(Arc::new(ContextualUserFragment::into(
                    AppsInstructions,
                )))),
            (true, false) => self.fragment.store(None),
            (false, false) | (true, true) => {}
        }
    }

    fn fragment(&self) -> Option<ResponseItem> {
        self.fragment
            .load_full()
            .map(|fragment| (*fragment).clone())
    }
}

impl Session {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "MCP app context rendering reads through the session-owned manager guard"
    )]
    pub(crate) async fn project_current_apps_instructions(
        &self,
        turn_context: &TurnContext,
        prompt_input: Vec<ResponseItem>,
    ) -> Vec<ResponseItem> {
        let available =
            if turn_context.config.include_apps_instructions && turn_context.apps_enabled() {
                let mcp_connection_manager = self.services.mcp_connection_manager.read().await;
                let accessible_and_enabled_connectors =
                    connectors::list_accessible_and_enabled_connectors_from_manager(
                        &mcp_connection_manager,
                        &turn_context.config,
                    )
                    .await;
                AppsInstructions::from_connectors(&accessible_and_enabled_connectors).is_some()
            } else {
                false
            };
        self.apps_context.update(available);

        canonicalize_apps_instructions(prompt_input, self.apps_context.fragment())
    }
}

fn canonicalize_apps_instructions(
    mut prompt_input: Vec<ResponseItem>,
    current_fragment: Option<ResponseItem>,
) -> Vec<ResponseItem> {
    prompt_input.retain_mut(|item| {
        let ResponseItem::Message { role, content, .. } = item else {
            return true;
        };
        if role != "developer" {
            return true;
        }
        content.retain_mut(|content_item| {
            let ContentItem::InputText { text } = content_item else {
                return true;
            };
            strip_apps_instruction_blocks(text);
            !text.trim().is_empty()
        });
        !content.is_empty()
    });

    if let Some(current_fragment) = current_fragment {
        let insertion_index = prompt_input
            .iter()
            .position(|item| {
                matches!(
                    crate::event_mapping::parse_turn_item(item),
                    Some(TurnItem::UserMessage(_))
                )
            })
            .or_else(|| {
                prompt_input.iter().position(|item| {
                    matches!(
                        item,
                        ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. }
                    )
                })
            })
            .unwrap_or(prompt_input.len());
        prompt_input.insert(insertion_index, current_fragment);
    }

    prompt_input
}

fn strip_apps_instruction_blocks(text: &mut String) {
    while let Some(start) = text.find(APPS_INSTRUCTIONS_OPEN_TAG) {
        let body_start = start + APPS_INSTRUCTIONS_OPEN_TAG.len();
        let Some(close_offset) = text[body_start..].find(APPS_INSTRUCTIONS_CLOSE_TAG) else {
            break;
        };
        let end = body_start + close_offset + APPS_INSTRUCTIONS_CLOSE_TAG.len();
        text.replace_range(start..end, "");
    }
}
