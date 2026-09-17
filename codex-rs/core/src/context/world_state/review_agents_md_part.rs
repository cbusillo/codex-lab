//! Typed, bounded AGENTS.md delivery for Background Review.

use super::PreviousSectionState;
use super::WorldStateSection;
use crate::context::ContextualUserFragment;
use codex_extension_api::MAX_WORLD_STATE_SECTION_BYTES;
use codex_protocol::models::ContentItemKind;
use serde::Deserialize;
use serde::Serialize;

pub(crate) const REVIEW_AGENTS_MD_PARTS: usize = 8;
pub(crate) const REVIEW_AGENTS_MD_KIND: &str = "background_review.agents_md_part";
pub(crate) const REVIEW_AGENTS_MD_TOTAL_BYTES: usize = 40 * 1024;
const REVIEW_AGENTS_MD_PART_IDS: [&str; REVIEW_AGENTS_MD_PARTS] = [
    "background_review_agents_md_part_0",
    "background_review_agents_md_part_1",
    "background_review_agents_md_part_2",
    "background_review_agents_md_part_3",
    "background_review_agents_md_part_4",
    "background_review_agents_md_part_5",
    "background_review_agents_md_part_6",
    "background_review_agents_md_part_7",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ReviewAgentsMdSnapshot {
    pub(crate) ordinal: usize,
    pub(crate) total: usize,
    pub(crate) digest: String,
    pub(crate) text: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ReviewAgentsMdPart<const N: usize> {
    snapshot: ReviewAgentsMdSnapshot,
}

impl<const N: usize> ReviewAgentsMdPart<N> {
    pub(crate) fn new(snapshot: ReviewAgentsMdSnapshot) -> Self {
        assert_eq!(
            snapshot.ordinal, N,
            "review AGENTS.md part ID must match its ordinal"
        );
        Self { snapshot }
    }
}

impl<const N: usize> WorldStateSection for ReviewAgentsMdPart<N> {
    const ID: &'static str = REVIEW_AGENTS_MD_PART_IDS[N];
    type Snapshot = ReviewAgentsMdSnapshot;

    fn snapshot(&self) -> Self::Snapshot {
        self.snapshot.clone()
    }

    fn max_rendered_bytes(&self) -> usize {
        MAX_WORLD_STATE_SECTION_BYTES
    }

    fn has_retained_fragment_matcher() -> bool {
        true
    }

    fn matches_retained_fragment(role: &str, text: &str) -> bool {
        // The generic retained matcher authenticates the full rendered text using
        // this section's persisted snapshot hash. Do not accept a marker-only
        // fallback: another part, a truncation envelope, or copied user text
        // must cause this exact part to be emitted again.
        let _ = (role, text);
        false
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        if matches!(previous, PreviousSectionState::Known(previous) if previous == &self.snapshot) {
            return None;
        }
        Some(Box::new(ReviewAgentsMdFragment(self.snapshot.clone())))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ReviewAgentsMdFragment(pub(crate) ReviewAgentsMdSnapshot);

impl ContextualUserFragment for ReviewAgentsMdFragment {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(REVIEW_AGENTS_MD_KIND.to_string())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn body(&self) -> String {
        format!(
            " part {}/{} digest={}\n<INSTRUCTIONS>\n{}\n",
            self.0.ordinal + 1,
            self.0.total,
            self.0.digest,
            self.0.text
        )
    }

    fn type_markers() -> (&'static str, &'static str) {
        (
            "# Background Review AGENTS.md instructions",
            "</BACKGROUND_REVIEW_AGENTS_MD>",
        )
    }
}
