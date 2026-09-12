//! Fail closed when Background Review cannot prove its instruction coverage.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;

use crate::agents_md::LoadedAgentsMd;
use crate::agents_md::agents_md_paths;
use crate::agents_md::candidate_filenames;
use crate::client_common::Prompt;
use crate::context::world_state::AgentsMdState;
use crate::context::world_state::PreviousSectionState;
use crate::context::world_state::WorldStateSection;
use crate::session::step_context::StepContext;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_utils_path_uri::PathUri;

/// Shared with the parent review task so a request denial persists its exact reason.
#[derive(Clone, Debug, Default)]
pub(crate) struct BackgroundReviewInstructionsGate {
    paths: Vec<PathUri>,
    failure: Arc<Mutex<Option<String>>>,
}

impl BackgroundReviewInstructionsGate {
    pub(crate) fn new(paths: Vec<PathUri>) -> Self {
        Self {
            paths,
            ..Self::default()
        }
    }

    pub(crate) fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) async fn authorize_request(
        &self,
        step: &StepContext,
        prompt: &Prompt,
    ) -> CodexResult<()> {
        let result = self.check(step, prompt).await;
        result.map_err(|reason| {
            let message = format!("Background Review cannot verify complete AGENTS.md instructions: {reason}. The review request was not sent.");
            *self.failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message.clone());
            CodexErr::InvalidRequest(message)
        })
    }

    async fn check(&self, step: &StepContext, prompt: &Prompt) -> Result<(), &'static str> {
        let loaded = step.loaded_agents_md.as_deref();
        if loaded.is_some_and(|loaded| !loaded.is_complete()) {
            return Err("instruction loading was truncated or failed");
        }
        if self.paths.is_empty() {
            return Err("the exact changed paths are unavailable");
        }
        let names = candidate_filenames(&step.turn.config);
        if self.paths.iter().any(|path| {
            path.to_path_buf()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| names.contains(&name))
        }) {
            return Err("the turn changes instruction files whose baseline scope is not available");
        }
        let environment = step
            .environments
            .primary()
            .ok_or("the review environment is unavailable")?;
        let filesystem = environment.environment.get_filesystem();
        let sandbox = (!environment
            .permission_profile()
            .file_system_sandbox_policy()
            .has_full_disk_read_access())
        .then(|| environment.sandbox_context(/*additional_permissions*/ None));
        let sources = loaded
            .into_iter()
            .flat_map(super::super::agents_md::LoadedAgentsMd::sources)
            .collect::<HashSet<_>>();
        let directories = self
            .paths
            .iter()
            .filter_map(PathUri::parent)
            .collect::<HashSet<_>>();
        for directory in directories {
            if directory.to_abs_path().is_err() {
                return Err("a changed path is outside the local review environment");
            }
            let required = agents_md_paths(
                &step.turn.config,
                &directory,
                filesystem.as_ref(),
                sandbox.as_ref(),
                codex_file_system::FindUpErrorPolicy::Propagate,
            )
            .await
            .map_err(|_| "applicable instruction files could not be discovered")?;
            if required.iter().any(|path| !sources.contains(path)) {
                return Err("applicable instruction files are missing from the review snapshot");
            }
        }
        validate_rendered_instructions(loaded, prompt)
    }
}

fn validate_rendered_instructions(
    loaded: Option<&LoadedAgentsMd>,
    prompt: &Prompt,
) -> Result<(), &'static str> {
    let state = AgentsMdState::new(loaded);
    let Some(initial) = state.render_diff(PreviousSectionState::Absent) else {
        return Ok(());
    };
    let initial = initial.render();
    if initial.len() > state.max_rendered_bytes() {
        return Err("the complete instruction fragment exceeds its context limit");
    }
    let replacement = state
        .render_diff(PreviousSectionState::Unknown)
        .map(|fragment| fragment.render());
    let present = prompt.input.iter().any(|item| match item {
        ResponseItem::Message {
            role,
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } if role == "user" => content.iter().enumerate().any(|(index, item)| {
            if metadata
                .content_item_kinds
                .as_ref()
                .and_then(|kinds| kinds.get(index))
                .is_none_or(|kind| kind.0 != "agents_md.instructions")
            {
                return false;
            }
            match item {
                ContentItem::InputText { text } => {
                    text == &initial
                        || replacement
                            .as_ref()
                            .is_some_and(|replacement| text == replacement)
                }
                _ => false,
            }
        }),
        _ => false,
    });
    if !present {
        return Err("the complete instruction fragment is absent from the final request");
    }
    Ok(())
}

#[cfg(test)]
#[path = "background_review_instructions_tests.rs"]
mod tests;
