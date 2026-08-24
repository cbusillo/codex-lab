use super::PreviousSectionState;
use super::WorldState;
use super::WorldStateSection;
use super::bounded_world_state_hash;
use super::ceil_char_boundary;
use super::floor_char_boundary;
use super::truncate_middle_to_byte_budget;
use crate::agents_md::LoadedAgentsMd;
use crate::context::ContextualUserFragment;
use crate::context::UserInstructions;
use serde::Deserialize;
use serde::Serialize;

const REPLACEMENT_NOTICE: &str = "These AGENTS.md instructions replace all previously provided \
    AGENTS.md instructions, including continuation parts.";
const REMOVAL_NOTICE: &str = "The previously provided AGENTS.md instructions, including \
    continuation parts, no longer apply.";
const AGENTS_MD_SHARD_COUNT: usize = 4;
pub(super) const AGENTS_MD_SHARD_RENDERED_MAX_BYTES: usize = 8 * 1024;
const LINE_SPLIT_SEARCH_BYTES: usize = 256;
const UTF8_SPLIT_SAFETY_BYTES: usize = (AGENTS_MD_SHARD_COUNT - 1) * 3;
#[derive(Clone, Debug, Default)]
pub(crate) struct AgentsMdState<const PART: usize = 1> {
    part: Option<AgentsMdPart>,
}

#[derive(Clone, Debug)]
struct AgentsMdPart {
    directory: Option<String>,
    text: String,
    document_hash: String,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct AgentsMdSnapshot {
    directory: Option<String>,
    text: Option<String>,
    #[serde(default)]
    document_hash: Option<String>,
    #[serde(default)]
    part: Option<usize>,
}

pub(crate) fn add_agents_md_sections(
    world_state: &mut WorldState,
    loaded: Option<&LoadedAgentsMd>,
) {
    let [part_1, part_2, part_3, part_4] = build_parts(loaded);
    world_state.add_section(AgentsMdState::<1> { part: part_1 });
    if let Some(part) = part_2 {
        world_state.add_section(AgentsMdState::<2> { part: Some(part) });
    }
    if let Some(part) = part_3 {
        world_state.add_section(AgentsMdState::<3> { part: Some(part) });
    }
    if let Some(part) = part_4 {
        world_state.add_section(AgentsMdState::<4> { part: Some(part) });
    }
}

fn build_parts(loaded: Option<&LoadedAgentsMd>) -> [Option<AgentsMdPart>; AGENTS_MD_SHARD_COUNT] {
    let Some(instructions) = loaded.map(LoadedAgentsMd::contextual_user_fragment) else {
        return std::array::from_fn(|_| None);
    };
    let document_hash = bounded_world_state_hash(
        "agents-md-document",
        &format!("{:?}\0{}", instructions.directory, instructions.text),
    );
    let capacities = std::array::from_fn(|index| {
        shard_content_capacity(index + 1, instructions.directory.as_deref())
    });
    let aggregate_content_capacity = capacities
        .iter()
        .sum::<usize>()
        .saturating_sub(UTF8_SPLIT_SAFETY_BYTES);
    let text = truncate_middle_to_byte_budget(&instructions.text, aggregate_content_capacity);
    let shards = split_document(&text, capacities);

    std::array::from_fn(|index| {
        shards.get(index).map(|text| AgentsMdPart {
            directory: instructions.directory.clone(),
            text: text.clone(),
            document_hash: document_hash.clone(),
        })
    })
}

fn shard_content_capacity(part: usize, directory: Option<&str>) -> usize {
    let text = if part == 1 {
        format!("{REPLACEMENT_NOTICE}\n\n")
    } else {
        String::new()
    };
    AGENTS_MD_SHARD_RENDERED_MAX_BYTES.saturating_sub(
        AgentsMdFragment {
            part,
            directory: directory.map(str::to_string),
            text,
        }
        .render()
        .len(),
    )
}

fn split_document(text: &str, capacities: [usize; AGENTS_MD_SHARD_COUNT]) -> Vec<String> {
    let mut shards = Vec::new();
    let mut remainder = text;
    for (index, capacity) in capacities.into_iter().enumerate() {
        if remainder.is_empty() {
            break;
        }
        if remainder.len() <= capacity {
            shards.push(remainder.to_string());
            remainder = "";
            break;
        }
        let later_capacity = capacities[index + 1..].iter().sum::<usize>();
        let minimum_split =
            ceil_char_boundary(remainder, remainder.len().saturating_sub(later_capacity));
        let maximum_split = floor_char_boundary(remainder, remainder.len().min(capacity));
        let search_start = minimum_split.max(maximum_split.saturating_sub(LINE_SPLIT_SEARCH_BYTES));
        let preferred_split = remainder.as_bytes()[search_start..maximum_split]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|offset| search_start + offset + 1);
        let split = preferred_split.unwrap_or(maximum_split).max(minimum_split);
        shards.push(remainder[..split].to_string());
        remainder = &remainder[split..];
    }
    debug_assert!(remainder.is_empty());
    shards
}

impl<const PART: usize> WorldStateSection for AgentsMdState<PART> {
    const ID: &'static str = match PART {
        1 => "agents_md",
        2 => "agents_md_2",
        3 => "agents_md_3",
        4 => "agents_md_4",
        _ => panic!("AGENTS.md shard part must be between 1 and 4"),
    };
    type Snapshot = AgentsMdSnapshot;

    fn snapshot(&self) -> Self::Snapshot {
        match &self.part {
            Some(part) => AgentsMdSnapshot {
                directory: part.directory.clone(),
                text: Some(part.text.clone()),
                document_hash: Some(part.document_hash.clone()),
                part: Some(PART),
            },
            None => AgentsMdSnapshot::default(),
        }
    }

    fn max_rendered_bytes(&self) -> usize {
        AGENTS_MD_SHARD_RENDERED_MAX_BYTES
    }

    fn matches_legacy_fragment(role: &str, text: &str) -> bool {
        role == "user"
            && if PART == 1 {
                UserInstructions::matches_text(text)
            } else {
                AgentsMdFragment::matches_part(PART, text)
            }
    }

    fn has_retained_fragment_matcher() -> bool {
        true
    }

    fn matches_retained_fragment(role: &str, text: &str) -> bool {
        role == "user" && AgentsMdFragment::matches_part(PART, text)
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
        let fragment = match (&self.part, PART, previous_may_contain_instructions) {
            (Some(part), 1, true) => AgentsMdFragment {
                part: PART,
                directory: part.directory.clone(),
                text: format!("{REPLACEMENT_NOTICE}\n\n{}", part.text),
            },
            (Some(part), _, _) => AgentsMdFragment {
                part: PART,
                directory: part.directory.clone(),
                text: part.text.clone(),
            },
            (None, 1, true) => AgentsMdFragment {
                part: PART,
                directory: None,
                text: REMOVAL_NOTICE.to_string(),
            },
            (None, _, _) => return None,
        };
        Some(Box::new(fragment))
    }
}

struct AgentsMdFragment {
    part: usize,
    directory: Option<String>,
    text: String,
}

impl AgentsMdFragment {
    fn part_markers(part: usize) -> (&'static str, &'static str) {
        match part {
            1 => ("# AGENTS.md instructions", "</INSTRUCTIONS>"),
            2 => (
                "# AGENTS.md instructions (continuation part 2 of 4)",
                "</INSTRUCTIONS_PART_2>",
            ),
            3 => (
                "# AGENTS.md instructions (continuation part 3 of 4)",
                "</INSTRUCTIONS_PART_3>",
            ),
            4 => (
                "# AGENTS.md instructions (continuation part 4 of 4)",
                "</INSTRUCTIONS_PART_4>",
            ),
            _ => ("", ""),
        }
    }

    fn matches_part(part: usize, text: &str) -> bool {
        let (start_marker, end_marker) = Self::part_markers(part);
        let trimmed = text.trim_start();
        let trimmed_end = text.trim_end();
        trimmed
            .get(..start_marker.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(start_marker))
            && trimmed_end
                .get(trimmed_end.len().saturating_sub(end_marker.len())..)
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(end_marker))
    }
}

impl ContextualUserFragment for AgentsMdFragment {
    fn role(&self) -> &'static str {
        "user"
    }

    fn requires_separate_message(&self) -> bool {
        true
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::part_markers(self.part)
    }

    fn body(&self) -> String {
        let directory = self
            .directory
            .as_ref()
            .map(|directory| format!(" for {directory}"))
            .unwrap_or_default();
        format!("{directory}\n\n<INSTRUCTIONS>\n{}\n", self.text)
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }
}

#[cfg(test)]
#[path = "agents_md_tests.rs"]
mod tests;
