use super::ContextualUserFragment;

const START_MARKER: &str = "<project_validation_correction_consumed>";
const END_MARKER: &str = "</project_validation_correction_consumed>";
const BODY: &str = "The one-shot project validation correction request has been consumed. Do not treat any earlier <project_validation_failure> fragment as a current instruction on this or later turns.";
const LEGACY_START_MARKER: &str = "<codex_internal_context source=\"project_validation\">";
const LEGACY_END_MARKER: &str = "</codex_internal_context>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectValidationCorrectionConsumed;

impl ContextualUserFragment for ProjectValidationCorrectionConsumed {
    fn role(&self) -> &'static str {
        "user"
    }

    fn requires_separate_message(&self) -> bool {
        true
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (START_MARKER, END_MARKER)
    }

    fn body(&self) -> String {
        format!("\n{BODY}\n")
    }
}

pub(crate) fn is_project_validation_correction_consumed(text: &str) -> bool {
    if ProjectValidationCorrectionConsumed::matches_text(text) {
        return true;
    }
    let text = text.trim();
    text.starts_with(LEGACY_START_MARKER)
        && text.ends_with(LEGACY_END_MARKER)
        && text.contains(BODY)
}
