mod agents_md;
mod apps_instructions;
mod collaboration_mode;
mod compact_permissions;
mod context_window_guidance;
mod environment;
pub(crate) mod environment_limits;
mod environments_instructions;
mod model;
mod multi_agent_mode;
mod multi_agent_usage_hint;
mod permissions;
mod personality;
mod plugins_instructions;
mod realtime;
#[cfg(test)]
mod test_support;
mod tools;

use crate::context::ContextualUserFragment;
use codex_extension_api::MAX_WORLD_STATE_SECTION_BYTES;
use codex_extension_api::PreviousWorldStateSection;
use codex_extension_api::RenderedWorldStateFragment;
use codex_extension_api::WorldStateSectionContribution;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use indexmap::IndexMap;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Map;
use serde_json::Value;
use sha1::Digest;
use sha1::Sha1;
use std::collections::BTreeMap;
use std::fmt;

const MAX_WORLD_STATE_TOTAL_BYTES: usize = 64 * 1024;
const MIN_WORLD_STATE_SECTION_BYTES: usize = 256;
const MAX_WORLD_STATE_SECTION_COUNT: usize = 128;
const MAX_EXTENSION_WORLD_STATE_SECTION_COUNT: usize = 64;
const BOUNDED_WORLD_STATE_CLOSE_TAG: &str = "</bounded_world_state_section>";
const WORLD_STATE_TRUNCATION_NOTICE: &str = "\n…world-state content truncated…\n";

pub(crate) use agents_md::AgentsMdState;
pub(crate) use apps_instructions::AppsInstructionsState;
pub(crate) use collaboration_mode::CollaborationModeState;
pub(crate) use compact_permissions::CompactPermissionsState;
pub(crate) use context_window_guidance::ContextWindowGuidanceState;
pub(crate) use environment::EnvironmentsSnapshot;
pub(crate) use environment::EnvironmentsState;
pub(crate) use environments_instructions::EnvironmentsInstructionsState;
pub(crate) use model::ModelInstructionsState;
pub(crate) use multi_agent_mode::MultiAgentModeState;
pub(crate) use multi_agent_usage_hint::MultiAgentUsageHintState;
pub(crate) use permissions::PermissionsState;
pub(crate) use personality::PersonalityState;
pub(crate) use plugins_instructions::PluginsInstructionsState;
pub(crate) use realtime::RealtimeState;
pub(crate) use tools::ToolsState;

trait ErasedWorldStateSection: Send + Sync {
    fn snapshot(&self) -> Option<Value>;

    fn max_rendered_bytes(&self) -> usize;

    fn matches_legacy_fragment(&self, role: &str, text: &str) -> bool;

    fn has_retained_fragment_matcher(&self) -> bool;

    fn matches_retained_fragment(&self, role: &str, text: &str) -> bool;

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Value>,
    ) -> Option<Box<dyn ContextualUserFragment>>;
}

impl<S: WorldStateSection> ErasedWorldStateSection for S {
    fn snapshot(&self) -> Option<Value> {
        if !WorldStateSection::should_persist(self) {
            return None;
        }
        let mut snapshot = match serde_json::to_value(WorldStateSection::snapshot(self)) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                tracing::error!(
                    section_id = S::ID,
                    %err,
                    "failed to serialize world-state section snapshot"
                );
                return None;
            }
        };
        remove_null_object_fields(&mut snapshot);
        if snapshot.is_null() {
            tracing::error!(
                section_id = S::ID,
                "world-state section snapshot cannot be null"
            );
            return None;
        }
        Some(snapshot)
    }

    fn max_rendered_bytes(&self) -> usize {
        WorldStateSection::max_rendered_bytes(self)
    }

    fn matches_legacy_fragment(&self, role: &str, text: &str) -> bool {
        WorldStateSection::matches_current_legacy_fragment(self, role, text)
    }

    fn has_retained_fragment_matcher(&self) -> bool {
        S::has_retained_fragment_matcher()
    }

    fn matches_retained_fragment(&self, role: &str, text: &str) -> bool {
        S::matches_retained_fragment(role, text)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Value>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        let typed_snapshot;
        let previous = match previous {
            PreviousSectionState::Known(previous) => {
                match serde_json::from_value::<S::Snapshot>(previous.clone()) {
                    Ok(previous) => {
                        typed_snapshot = previous;
                        PreviousSectionState::Known(&typed_snapshot)
                    }
                    Err(err) => {
                        tracing::warn!(
                            section_id = S::ID,
                            %err,
                            "failed to restore world-state section snapshot"
                        );
                        PreviousSectionState::Unknown
                    }
                }
            }
            PreviousSectionState::Absent => PreviousSectionState::Absent,
            PreviousSectionState::Unknown => PreviousSectionState::Unknown,
        };
        WorldStateSection::render_diff(self, previous)
    }
}

struct ExtensionWorldStateSection(WorldStateSectionContribution);

impl ErasedWorldStateSection for ExtensionWorldStateSection {
    fn snapshot(&self) -> Option<Value> {
        let mut snapshot = self.0.snapshot().clone();
        remove_null_object_fields(&mut snapshot);
        (!snapshot.is_null()).then_some(snapshot)
    }

    fn max_rendered_bytes(&self) -> usize {
        MAX_WORLD_STATE_SECTION_BYTES
    }

    fn matches_legacy_fragment(&self, role: &str, text: &str) -> bool {
        self.0.matches_legacy_fragment(role, text)
    }

    fn has_retained_fragment_matcher(&self) -> bool {
        self.0.has_retained_fragment_matcher()
    }

    fn matches_retained_fragment(&self, role: &str, text: &str) -> bool {
        self.0.matches_retained_fragment(role, text)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Value>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        let previous = match previous {
            PreviousSectionState::Absent => PreviousWorldStateSection::Absent,
            PreviousSectionState::Unknown => PreviousWorldStateSection::Unknown,
            PreviousSectionState::Known(previous) => PreviousWorldStateSection::Known(previous),
        };
        self.0
            .render_diff(previous)
            .map(|fragment| Box::new(WorldStateContextFragment(fragment)) as _)
    }
}

struct WorldStateContextFragment(RenderedWorldStateFragment);

impl ContextualUserFragment for WorldStateContextFragment {
    fn role(&self) -> &'static str {
        self.0.role()
    }

    fn markers(&self) -> (&'static str, &'static str) {
        self.0.markers()
    }

    fn body(&self) -> String {
        self.0.body().to_string()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }
}

struct PendingWorldStateFragment {
    id: &'static str,
    state_hash: String,
    role: &'static str,
    requires_separate_message: bool,
    markers: (&'static str, &'static str),
    body: String,
    max_rendered_bytes: usize,
}

impl PendingWorldStateFragment {
    fn new(
        id: &'static str,
        state_hash: Option<String>,
        max_rendered_bytes: usize,
        fragment: Box<dyn ContextualUserFragment>,
    ) -> Self {
        let role = fragment.role();
        let requires_separate_message = fragment.requires_separate_message();
        let markers = fragment.markers();
        let body = fragment.body();
        let rendered = format!("{}{body}{}", markers.0, markers.1);
        Self {
            id,
            state_hash: state_hash
                .unwrap_or_else(|| bounded_world_state_hash("rendered", &rendered)),
            role,
            requires_separate_message,
            markers,
            body,
            max_rendered_bytes,
        }
    }

    fn rendered_byte_count(&self) -> usize {
        self.markers
            .0
            .len()
            .saturating_add(self.body.len())
            .saturating_add(self.markers.1.len())
    }

    fn render(&self) -> String {
        format!("{}{}{}", self.markers.0, self.body, self.markers.1)
    }
}

struct BoundedWorldStateFragment {
    role: &'static str,
    requires_separate_message: bool,
    markers: (&'static str, &'static str),
    body: String,
    original_byte_count: usize,
    rendered_byte_count: usize,
}

impl BoundedWorldStateFragment {
    fn new(fragment: PendingWorldStateFragment, max_bytes: usize) -> Self {
        let original_byte_count = fragment.rendered_byte_count();
        if original_byte_count <= max_bytes {
            return Self {
                role: fragment.role,
                requires_separate_message: fragment.requires_separate_message,
                markers: fragment.markers,
                body: fragment.body,
                original_byte_count,
                rendered_byte_count: original_byte_count,
            };
        }

        let rendered = fragment.render();
        let open_tag =
            bounded_world_state_open_tag(fragment.id, fragment.role, &fragment.state_hash);
        let generic_envelope_byte_count = open_tag
            .len()
            .saturating_add(BOUNDED_WORLD_STATE_CLOSE_TAG.len());
        assert!(
            generic_envelope_byte_count < MIN_WORLD_STATE_SECTION_BYTES,
            "bounded world-state envelope exceeds the minimum section budget"
        );
        let marked_envelope_byte_count = generic_envelope_byte_count
            .saturating_add(fragment.markers.0.len())
            .saturating_add(fragment.markers.1.len());
        let (markers, content, content_byte_budget) = if marked_envelope_byte_count < max_bytes {
            (
                fragment.markers,
                fragment.body.as_str(),
                max_bytes.saturating_sub(marked_envelope_byte_count),
            )
        } else {
            (
                ("", ""),
                rendered.as_str(),
                max_bytes.saturating_sub(generic_envelope_byte_count),
            )
        };
        let body = truncate_middle_to_byte_budget(content, content_byte_budget);
        let body = format!("{open_tag}{body}{BOUNDED_WORLD_STATE_CLOSE_TAG}");
        let rendered_byte_count = markers
            .0
            .len()
            .saturating_add(body.len())
            .saturating_add(markers.1.len());
        debug_assert!(rendered_byte_count <= max_bytes);
        Self {
            role: fragment.role,
            requires_separate_message: fragment.requires_separate_message,
            markers,
            body,
            original_byte_count,
            rendered_byte_count,
        }
    }

    fn was_truncated(&self) -> bool {
        self.rendered_byte_count < self.original_byte_count
    }
}

impl ContextualUserFragment for BoundedWorldStateFragment {
    fn role(&self) -> &'static str {
        self.role
    }

    fn requires_separate_message(&self) -> bool {
        self.requires_separate_message
    }

    fn markers(&self) -> (&'static str, &'static str) {
        self.markers
    }

    fn body(&self) -> String {
        self.body.clone()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }
}

fn allocate_world_state_budgets(fragments: &[PendingWorldStateFragment]) -> Vec<usize> {
    let capped_byte_counts = fragments
        .iter()
        .map(|fragment| {
            fragment
                .rendered_byte_count()
                .min(fragment.max_rendered_bytes)
        })
        .collect::<Vec<_>>();
    if capped_byte_counts.iter().sum::<usize>() <= MAX_WORLD_STATE_TOTAL_BYTES {
        return capped_byte_counts;
    }

    let mut budgets = capped_byte_counts
        .iter()
        .map(|byte_count| (*byte_count).min(MIN_WORLD_STATE_SECTION_BYTES))
        .collect::<Vec<_>>();
    let mut remaining_bytes =
        MAX_WORLD_STATE_TOTAL_BYTES.saturating_sub(budgets.iter().sum::<usize>());

    while remaining_bytes > 0 {
        let active_indices = budgets
            .iter()
            .zip(&capped_byte_counts)
            .enumerate()
            .filter_map(|(index, (budget, byte_count))| (budget < byte_count).then_some(index))
            .collect::<Vec<_>>();
        if active_indices.is_empty() {
            break;
        }
        let share = (remaining_bytes / active_indices.len()).max(1);
        let mut distributed_bytes = 0usize;
        for index in active_indices {
            let available_bytes = capped_byte_counts[index].saturating_sub(budgets[index]);
            let allocated_bytes = available_bytes.min(share).min(remaining_bytes);
            budgets[index] = budgets[index].saturating_add(allocated_bytes);
            remaining_bytes = remaining_bytes.saturating_sub(allocated_bytes);
            distributed_bytes = distributed_bytes.saturating_add(allocated_bytes);
            if remaining_bytes == 0 {
                break;
            }
        }
        if distributed_bytes == 0 {
            break;
        }
    }

    budgets
}

fn truncate_middle_to_byte_budget(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    if max_bytes <= WORLD_STATE_TRUNCATION_NOTICE.len() {
        let end = floor_char_boundary(WORLD_STATE_TRUNCATION_NOTICE, max_bytes);
        return WORLD_STATE_TRUNCATION_NOTICE[..end].to_string();
    }

    let retained_byte_count = max_bytes.saturating_sub(WORLD_STATE_TRUNCATION_NOTICE.len());
    let prefix_end = floor_char_boundary(text, retained_byte_count / 2);
    let suffix_start = ceil_char_boundary(
        text,
        text.len()
            .saturating_sub(retained_byte_count.saturating_sub(prefix_end)),
    );
    format!(
        "{}{}{}",
        &text[..prefix_end],
        WORLD_STATE_TRUNCATION_NOTICE,
        &text[suffix_start..]
    )
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index = index.saturating_sub(1);
    }
    index
}

fn ceil_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index = index.saturating_add(1);
    }
    index
}

fn bounded_world_state_open_tag(section_id: &str, role: &str, state_hash: &str) -> String {
    let section_hash = bounded_world_state_hash("section", section_id);
    format!(
        "<bounded_world_state_section section=\"{section_hash}\" role=\"{role}\" state=\"{state_hash}\">"
    )
}

fn bounded_world_state_hash(domain: &str, value: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(b"codex-bounded-world-state-v1\0");
    hash_component(&mut hasher, domain);
    hash_component(&mut hasher, value);
    format!("{:x}", hasher.finalize())
}

fn bounded_world_state_state_hash(state: &Value) -> String {
    bounded_world_state_hash("state", &state.to_string())
}

fn matches_bounded_world_state_fragment(
    section_id: &str,
    state: &Value,
    role: &str,
    text: &str,
) -> bool {
    let state_hash = bounded_world_state_state_hash(state);
    let open_tag = bounded_world_state_open_tag(section_id, role, &state_hash);
    text.contains(&open_tag) && text.contains(BOUNDED_WORLD_STATE_CLOSE_TAG)
}

/// What is known about a section's previously model-visible state.
pub(crate) enum PreviousSectionState<'a, T> {
    /// No persisted snapshot or matching fragment exists in retained history.
    Absent,
    /// Retained history contains the section, but its typed snapshot is unavailable.
    Unknown,
    /// The exact persisted snapshot is available.
    Known(&'a T),
}

/// A typed portion of the state visible to the model.
///
/// Implementations own how their current state is rendered relative to an
/// earlier snapshot of the same section. `ID` is persisted in rollouts and
/// must remain stable. `Snapshot` should contain only the comparison data
/// needed to decide what the model must be told next, and must not serialize
/// to null because merge-patch nulls represent deletion. Sections migrated
/// from older context can recognize their previous fragments through
/// `matches_legacy_fragment`.
pub(crate) trait WorldStateSection: Send + Sync + 'static {
    const ID: &'static str;
    type Snapshot: DeserializeOwned + Serialize;

    fn snapshot(&self) -> Self::Snapshot;

    /// Whether the section contributes comparison state to persisted rollouts.
    fn should_persist(&self) -> bool {
        true
    }

    /// Maximum rendered size for this section before bounded truncation.
    fn max_rendered_bytes(&self) -> usize {
        MAX_WORLD_STATE_SECTION_BYTES
    }

    fn matches_legacy_fragment(_role: &str, _text: &str) -> bool {
        false
    }

    /// Recognizes legacy fragments whose identity depends on this section's current value.
    fn matches_current_legacy_fragment(&self, role: &str, text: &str) -> bool {
        Self::matches_legacy_fragment(role, text)
    }

    /// Whether retained history must still contain this section's rendered fragment.
    fn has_retained_fragment_matcher() -> bool {
        false
    }

    /// Recognizes this section's rendered fragment in retained model history.
    fn matches_retained_fragment(_role: &str, _text: &str) -> bool {
        false
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>>;
}

/// Stable fingerprint of a model-visible World State fragment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub(crate) struct WorldStateHash(String);

impl WorldStateHash {
    pub(crate) fn from_fragment(fragment: &(impl ContextualUserFragment + ?Sized)) -> Self {
        let mut hasher = Sha1::new();
        hasher.update(b"codex-world-state-fragment-v1\0");
        hash_component(&mut hasher, fragment.role());
        hash_component(&mut hasher, &fragment.render());
        Self(format!("{:x}", hasher.finalize()))
    }
}

fn hash_component(hasher: &mut Sha1, value: &str) {
    let value = value.replace("\r\n", "\n");
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

/// Live model-visible state, keyed by the same stable section IDs used in rollouts.
#[derive(Default)]
pub(crate) struct WorldState {
    sections: IndexMap<&'static str, Box<dyn ErasedWorldStateSection>>,
    extension_section_count: usize,
}

/// Compact comparison state for each model-visible world-state section.
#[derive(Clone, Debug, Default, PartialEq, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub(crate) struct WorldStateSnapshot {
    sections: BTreeMap<String, Value>,
}

impl WorldStateSnapshot {
    /// Seed a baseline from a rollout `TurnContextItem` for rollouts recorded
    /// before world-state items were persisted.
    ///
    /// Only the sections that the turn context durably recorded are seeded; the
    /// rest keep the history-based fallback used when no baseline is available.
    pub(crate) fn from_legacy_turn_context_item(
        turn_context_item: &codex_protocol::protocol::TurnContextItem,
    ) -> Option<Self> {
        let environments = EnvironmentsSnapshot::from_turn_context_item(turn_context_item)?;
        let environments = serde_json::to_value(environments).ok()?;
        Some(Self {
            sections: BTreeMap::from([(EnvironmentsState::ID.to_string(), environments)]),
        })
    }

    pub(crate) fn into_value(self) -> Value {
        Value::Object(self.sections.into_iter().collect())
    }

    /// Returns the RFC 7386 merge patch that advances `previous` to `self`.
    pub(crate) fn merge_patch_from(&self, previous: &Self) -> Option<Value> {
        let previous = Value::Object(previous.sections.clone().into_iter().collect());
        let current = Value::Object(self.sections.clone().into_iter().collect());
        create_merge_patch(&previous, &current)
    }

    pub(crate) fn apply_merge_patch(&mut self, patch: &Value) -> serde_json::Result<()> {
        let mut current = self.clone().into_value();
        apply_merge_patch_value(&mut current, patch);
        *self = serde_json::from_value(current)?;
        Ok(())
    }
}

impl fmt::Debug for WorldState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldState")
            .field("section_count", &self.sections.len())
            .field("extension_section_count", &self.extension_section_count)
            .finish()
    }
}

impl WorldState {
    pub(crate) fn add_section<S: WorldStateSection>(&mut self, section: S) {
        let id = S::ID;
        assert!(
            !self.sections.contains_key(id),
            "duplicate world-state section ID: {id}"
        );
        assert!(
            self.sections.len() < MAX_WORLD_STATE_SECTION_COUNT,
            "world-state section count exceeds {MAX_WORLD_STATE_SECTION_COUNT}"
        );
        self.sections.insert(id, Box::new(section));
    }

    pub(crate) fn add_extension_section(&mut self, section: WorldStateSectionContribution) {
        let id = section.id();
        assert!(
            !self.sections.contains_key(id),
            "duplicate world-state section ID: {id}"
        );
        if self.extension_section_count >= MAX_EXTENSION_WORLD_STATE_SECTION_COUNT
            || self.sections.len() >= MAX_WORLD_STATE_SECTION_COUNT
        {
            tracing::warn!(
                section_id = id,
                extension_section_count = self.extension_section_count,
                "ignored extension world-state section after reaching the section-count limit"
            );
            return;
        }
        let section = Box::new(ExtensionWorldStateSection(section));
        if id == "host_skills"
            && let Some(index) = self.sections.get_index_of(PermissionsState::ID)
        {
            self.sections.shift_insert(index, id, section);
        } else {
            self.sections.insert(id, section);
        }
        self.extension_section_count = self.extension_section_count.saturating_add(1);
    }

    pub(crate) fn snapshot(&self) -> WorldStateSnapshot {
        WorldStateSnapshot {
            sections: self
                .sections
                .iter()
                .filter_map(|(id, section)| {
                    section
                        .snapshot()
                        .map(|snapshot| ((*id).to_string(), snapshot))
                })
                .collect(),
        }
    }

    /// Renders every section as new, without any known previous state.
    pub(crate) fn render_full(&self) -> Vec<Box<dyn ContextualUserFragment>> {
        self.render_with(|_, _| PreviousSectionState::Absent)
    }

    /// Renders each section against the exact persisted snapshot when available.
    pub(crate) fn render_diff(
        &self,
        previous: &WorldStateSnapshot,
    ) -> Vec<Box<dyn ContextualUserFragment>> {
        self.render_with(|id, _| match previous.sections.get(id) {
            Some(previous) => PreviousSectionState::Known(previous),
            None => PreviousSectionState::Absent,
        })
    }

    /// Falls back to retained model history when no exact persisted snapshot is available.
    pub(crate) fn render_history_diff(
        &self,
        previous: Option<&WorldStateSnapshot>,
        items: &[ResponseItem],
    ) -> Vec<Box<dyn ContextualUserFragment>> {
        self.render_with(|id, section| {
            if let Some(previous) = previous.and_then(|previous| previous.sections.get(id)) {
                if section.has_retained_fragment_matcher()
                    && !has_retained_fragment(items, id, previous, section)
                {
                    PreviousSectionState::Absent
                } else {
                    PreviousSectionState::Known(previous)
                }
            } else if has_legacy_fragment(items, section) {
                PreviousSectionState::Unknown
            } else {
                PreviousSectionState::Absent
            }
        })
    }

    fn render_with<'a>(
        &self,
        mut previous: impl FnMut(&str, &dyn ErasedWorldStateSection) -> PreviousSectionState<'a, Value>,
    ) -> Vec<Box<dyn ContextualUserFragment>> {
        let fragments = self
            .sections
            .iter()
            .filter_map(|(id, section)| {
                let previous = previous(id, section.as_ref());
                section.render_diff(previous).map(|fragment| {
                    PendingWorldStateFragment::new(
                        id,
                        section
                            .snapshot()
                            .as_ref()
                            .map(bounded_world_state_state_hash),
                        section.max_rendered_bytes(),
                        fragment,
                    )
                })
            })
            .collect::<Vec<_>>();
        assert!(
            fragments.len() <= MAX_WORLD_STATE_SECTION_COUNT,
            "rendered world-state section count exceeds {MAX_WORLD_STATE_SECTION_COUNT}"
        );
        let budgets = allocate_world_state_budgets(&fragments);

        fragments
            .into_iter()
            .zip(budgets)
            .map(|(fragment, section_byte_budget)| {
                let section_id = fragment.id;
                let fragment = BoundedWorldStateFragment::new(fragment, section_byte_budget);
                if fragment.was_truncated() {
                    tracing::warn!(
                        section_id,
                        original_byte_count = fragment.original_byte_count,
                        rendered_byte_count = fragment.rendered_byte_count,
                        section_byte_budget,
                        "truncated world-state section to its model-context budget"
                    );
                }
                Box::new(fragment) as Box<dyn ContextualUserFragment>
            })
            .collect()
    }
}

fn has_retained_fragment(
    items: &[ResponseItem],
    section_id: &str,
    state: &Value,
    section: &dyn ErasedWorldStateSection,
) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            ResponseItem::Message { role, content, .. }
                if content.iter().any(|content| {
                    matches!(
                        content,
                        ContentItem::InputText { text }
                            if matches_bounded_world_state_fragment(section_id, state, role, text)
                                || section.matches_retained_fragment(role, text)
                    )
                })
        )
    })
}

fn has_legacy_fragment(items: &[ResponseItem], section: &dyn ErasedWorldStateSection) -> bool {
    items.iter().any(|item| {
        matches!(
            item,
            ResponseItem::Message { role, content, .. }
                if content.iter().any(|content| {
                    matches!(
                        content,
                        ContentItem::InputText { text }
                            if section.matches_legacy_fragment(role, text)
                    )
                })
        )
    })
}

fn remove_null_object_fields(value: &mut Value) {
    // RFC 7386 reserves object-valued nulls for deletion, but arrays are replaced whole.
    match value {
        Value::Object(values) => {
            values.retain(|_, value| !value.is_null());
            values.values_mut().for_each(remove_null_object_fields);
        }
        Value::Array(_) => {}
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn create_merge_patch(previous: &Value, current: &Value) -> Option<Value> {
    if previous == current {
        return None;
    }

    let Value::Object(current) = current else {
        return Some(current.clone());
    };
    let previous = previous.as_object();
    let mut patch = Map::new();

    if let Some(previous) = previous {
        for key in previous.keys() {
            if !current.contains_key(key) {
                patch.insert(key.clone(), Value::Null);
            }
        }
    }

    for (key, current_value) in current {
        let Some(previous_value) = previous.and_then(|previous| previous.get(key)) else {
            patch.insert(key.clone(), current_value.clone());
            continue;
        };
        if let Some(value_patch) = create_merge_patch(previous_value, current_value) {
            patch.insert(key.clone(), value_patch);
        }
    }

    Some(Value::Object(patch))
}

fn apply_merge_patch_value(target: &mut Value, patch: &Value) {
    let Value::Object(patch) = patch else {
        target.clone_from(patch);
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    if let Value::Object(target) = target {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                apply_merge_patch_value(target.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
    }
}

#[cfg(test)]
#[path = "world_state_tests.rs"]
mod tests;
