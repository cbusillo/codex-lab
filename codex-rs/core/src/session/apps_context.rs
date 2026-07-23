use std::sync::Mutex as StdMutex;

use codex_protocol::models::ContentItem;
use codex_protocol::protocol::APPS_INSTRUCTIONS_CLOSE_TAG;
use codex_protocol::protocol::APPS_INSTRUCTIONS_OPEN_TAG;

use super::*;
use crate::connectors;
use crate::context::APPS_UPDATE_CLOSE_TAG;
use crate::context::APPS_UPDATE_OPEN_TAG;
use crate::context::AppsAvailability;
use crate::context::AppsAvailabilityUpdate;
use crate::context::AppsInstructions;
use crate::context::ContextualUserFragment;
use crate::context_manager::ContextManager;
use crate::execution_account::ExecutionAccountCacheIdentity;

pub(super) struct AppsContext {
    state: StdMutex<AppsContextState>,
}

struct AppsContextState {
    availability: Option<AppsAvailability>,
    mcp_cache_identity: ExecutionAccountCacheIdentity,
}

impl AppsContext {
    pub(super) fn new(mcp_cache_identity: ExecutionAccountCacheIdentity) -> Self {
        Self {
            state: StdMutex::new(AppsContextState {
                availability: None,
                mcp_cache_identity,
            }),
        }
    }

    fn update(&self, availability: AppsAvailability) -> AppsContextChange {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.availability.replace(availability) {
            Some(AppsAvailability::Available) if availability == AppsAvailability::Unavailable => {
                AppsContextChange::BecameUnavailable
            }
            None | Some(AppsAvailability::Unavailable | AppsAvailability::Available) => {
                AppsContextChange::None
            }
        }
    }

    fn update_from_history(&self, items: &[ResponseItem]) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .availability = apps_availability_from_items(items);
    }

    fn mcp_cache_identity_matches(&self, current: &ExecutionAccountCacheIdentity) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mcp_cache_identity
            == *current
    }

    pub(super) fn set_mcp_cache_identity(&self, current: ExecutionAccountCacheIdentity) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mcp_cache_identity = current;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppsContextChange {
    None,
    BecameUnavailable,
}

impl AppsContextChange {
    fn into_response_item(self) -> Option<ResponseItem> {
        match self {
            Self::None => None,
            Self::BecameUnavailable => Some(ContextualUserFragment::into(
                AppsAvailabilityUpdate::new(AppsAvailability::Unavailable),
            )),
        }
    }
}

impl Session {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "MCP app context rendering reads through the session-owned manager guard"
    )]
    async fn refresh_apps_context(&self, turn_context: &TurnContext) -> AppsContextChange {
        let availability =
            if turn_context.config.include_apps_instructions && turn_context.apps_enabled() {
                let accessible_and_enabled_connectors =
                    connectors::list_accessible_and_enabled_connectors_from_runtime(
                        self.services.mcp_runtime.as_ref(),
                        &turn_context.config,
                    )
                    .await;
                if AppsInstructions::from_connectors(&accessible_and_enabled_connectors).is_some() {
                    AppsAvailability::Available
                } else {
                    AppsAvailability::Unavailable
                }
            } else {
                AppsAvailability::Unavailable
            };
        self.apps_context.update(availability)
    }

    pub(super) async fn refresh_apps_context_item(
        &self,
        turn_context: &TurnContext,
    ) -> Option<ResponseItem> {
        self.refresh_apps_context(turn_context)
            .await
            .into_response_item()
    }

    pub(crate) async fn prepare_model_visible_history(
        &self,
        turn_context: &TurnContext,
    ) -> ContextManager {
        let history = self.clone_history().await;
        self.apps_context.update_from_history(history.raw_items());
        if let Some(item) = self.refresh_apps_context_item(turn_context).await {
            self.record_conversation_items(turn_context, std::slice::from_ref(&item))
                .await;
            return self.clone_history().await;
        }
        history
    }

    pub(crate) async fn ensure_mcp_manager_for_execution_account(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
    ) -> Arc<TurnContext> {
        let current = self.services.execution_account.cache_identity();
        if self.apps_context.mcp_cache_identity_matches(&current) {
            return Arc::clone(turn_context);
        }
        self.refresh_turn_context_for_execution_account(turn_context)
            .await
    }
}

pub(super) fn migrate_legacy_apps_instructions(items: &mut Vec<ResponseItem>) {
    let mut kept_legacy_block = false;

    items.retain_mut(|item| {
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
            canonicalize_legacy_apps_instruction_blocks(text, &mut kept_legacy_block);
            !text.trim().is_empty()
        });
        !content.is_empty()
    });
}

pub(super) fn update_apps_context_from_history(apps_context: &AppsContext, items: &[ResponseItem]) {
    apps_context.update_from_history(items);
}

fn canonicalize_legacy_apps_instruction_blocks(text: &mut String, kept_legacy_block: &mut bool) {
    let canonical = AppsInstructions.render();
    let mut search_start = 0;
    while let Some(relative_start) = text[search_start..].find(APPS_INSTRUCTIONS_OPEN_TAG) {
        let start = search_start + relative_start;
        let body_start = start + APPS_INSTRUCTIONS_OPEN_TAG.len();
        let Some(close_offset) = text[body_start..].find(APPS_INSTRUCTIONS_CLOSE_TAG) else {
            break;
        };
        let end = body_start + close_offset + APPS_INSTRUCTIONS_CLOSE_TAG.len();
        if *kept_legacy_block {
            text.replace_range(start..end, "");
            search_start = start;
        } else {
            *kept_legacy_block = true;
            if text[start..end] != canonical {
                text.replace_range(start..end, &canonical);
            }
            search_start = start + canonical.len();
        }
    }
}

fn apps_availability_from_items(items: &[ResponseItem]) -> Option<AppsAvailability> {
    let mut availability = None;
    for item in items {
        let ResponseItem::Message { role, content, .. } = item else {
            continue;
        };
        if role != "developer" {
            continue;
        }
        for content_item in content {
            let ContentItem::InputText { text } = content_item else {
                continue;
            };
            if let Some(latest) = latest_apps_availability_in_text(text) {
                availability = Some(latest);
            }
        }
    }
    availability
}

fn latest_apps_availability_in_text(text: &str) -> Option<AppsAvailability> {
    let legacy = latest_complete_block_start(
        text,
        APPS_INSTRUCTIONS_OPEN_TAG,
        APPS_INSTRUCTIONS_CLOSE_TAG,
    )
    .map(|position| (position, AppsAvailability::Available));
    let update = latest_complete_block(text, APPS_UPDATE_OPEN_TAG, APPS_UPDATE_CLOSE_TAG).and_then(
        |(position, body)| {
            AppsAvailabilityUpdate::availability_from_body(body)
                .map(|availability| (position, availability))
        },
    );
    match (legacy, update) {
        (Some(legacy), Some(update)) => Some(if legacy.0 > update.0 {
            legacy.1
        } else {
            update.1
        }),
        (Some((_, availability)), None) | (None, Some((_, availability))) => Some(availability),
        (None, None) => None,
    }
}

fn latest_complete_block_start(text: &str, open: &str, close: &str) -> Option<usize> {
    latest_complete_block(text, open, close).map(|(position, _)| position)
}

fn latest_complete_block<'a>(text: &'a str, open: &str, close: &str) -> Option<(usize, &'a str)> {
    let mut latest = None;
    let mut search_start = 0;
    while let Some(relative_start) = text[search_start..].find(open) {
        let start = search_start + relative_start;
        let body_start = start + open.len();
        let Some(close_offset) = text[body_start..].find(close) else {
            break;
        };
        let body_end = body_start + close_offset;
        latest = Some((start, &text[body_start..body_end]));
        search_start = body_end + close.len();
    }
    latest
}
