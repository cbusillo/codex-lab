use super::MIN_WORLD_STATE_SECTION_BYTES;
use super::PreviousSectionState;
use super::WorldStateSection;
use crate::agents_md::LoadedAgentsMd;
use crate::config::AGENTS_MD_MAX_BYTES;
use crate::context::ContextualUserFragment;
use crate::context::UserInstructions;
use serde::Deserialize;
use serde::Serialize;

const REPLACEMENT_NOTICE: &str =
    "These AGENTS.md instructions replace all previously provided AGENTS.md instructions.";
const REMOVAL_NOTICE: &str = "The previously provided AGENTS.md instructions no longer apply.";

/// The AGENTS.md instructions currently visible to the model.
#[derive(Clone, Debug)]
pub(crate) struct AgentsMdState {
    instructions: Option<UserInstructions>,
    max_rendered_bytes: usize,
}

/// Persisted model-visible AGENTS.md state, without filesystem provenance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct AgentsMdSnapshot {
    directory: Option<String>,
    text: Option<String>,
}

impl AgentsMdState {
    pub(crate) fn new(loaded: Option<&LoadedAgentsMd>, project_doc_max_bytes: usize) -> Self {
        Self {
            instructions: loaded.map(LoadedAgentsMd::contextual_user_fragment),
            max_rendered_bytes: project_doc_max_bytes
                .clamp(MIN_WORLD_STATE_SECTION_BYTES, AGENTS_MD_MAX_BYTES),
        }
    }
}

impl Default for AgentsMdState {
    fn default() -> Self {
        Self::new(/*loaded*/ None, AGENTS_MD_MAX_BYTES)
    }
}

impl WorldStateSection for AgentsMdState {
    const ID: &'static str = "agents_md";
    type Snapshot = AgentsMdSnapshot;

    fn snapshot(&self) -> Self::Snapshot {
        match &self.instructions {
            Some(instructions) => AgentsMdSnapshot {
                directory: instructions.directory.clone(),
                text: Some(instructions.text.clone()),
            },
            None => AgentsMdSnapshot::default(),
        }
    }

    fn max_rendered_bytes(&self) -> usize {
        self.max_rendered_bytes
    }

    fn matches_legacy_fragment(role: &str, text: &str) -> bool {
        role == "user" && UserInstructions::matches_text(text)
    }

    fn has_retained_fragment_matcher() -> bool {
        true
    }

    fn matches_retained_fragment(role: &str, text: &str) -> bool {
        Self::matches_legacy_fragment(role, text)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        let current = self.snapshot();
        if matches!(previous, PreviousSectionState::Known(previous) if previous == &current) {
            return None;
        }

        let previous_may_contain_instructions = match previous {
            PreviousSectionState::Known(previous) => previous.text.is_some(),
            PreviousSectionState::Unknown => true,
            PreviousSectionState::Absent => false,
        };
        let instructions = match (&self.instructions, previous_may_contain_instructions) {
            (Some(instructions), true) => UserInstructions {
                directory: instructions.directory.clone(),
                text: format!("{REPLACEMENT_NOTICE}\n\n{}", instructions.text),
            },
            (Some(instructions), false) => instructions.clone(),
            (None, true) => UserInstructions {
                directory: None,
                text: REMOVAL_NOTICE.to_string(),
            },
            (None, false) => return None,
        };
        Some(Box::new(instructions))
    }
}

#[cfg(test)]
#[path = "agents_md_tests.rs"]
mod tests;
