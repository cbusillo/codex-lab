use codex_protocol::protocol::ProjectValidationCompletedEvent;
use codex_protocol::protocol::ProjectValidationStatus;
use codex_utils_string::take_bytes_at_char_boundary;

use super::ContextualUserFragment;

const START_MARKER: &str = "<project_validation_failure>";
const END_MARKER: &str = "</project_validation_failure>";
const BODY_PADDING: &str = "\n";
const MAX_COMMAND_BYTES: usize = 192;
const MAX_RENDERED_BYTES: usize = 960;
const TRUNCATED_MARKER: &str = "\n… project validation feedback truncated …\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectValidationFailure {
    body: String,
}

impl ProjectValidationFailure {
    pub(crate) fn from_event(event: &ProjectValidationCompletedEvent) -> Option<Self> {
        if event.status != ProjectValidationStatus::ActionableFailure {
            return None;
        }

        let exit_code = event
            .exit_code
            .map_or_else(|| "unknown".to_string(), |code| code.to_string());
        let output_truncated = if event.output_truncated { "yes" } else { "no" };
        let max_body_bytes = MAX_RENDERED_BYTES
            .saturating_sub(START_MARKER.len())
            .saturating_sub(END_MARKER.len())
            .saturating_sub(BODY_PADDING.len().saturating_mul(2));
        let instruction = "Project validation failed. Fix the actionable failure below. Do not run this configured command yourself; the runtime will rerun it once after this correction cycle.";
        let metadata = format!(
            "\nExit code: {exit_code}\nEvent output truncated: {output_truncated}\nOutput:\n"
        );
        let body_overhead = instruction.len() + "\nCommand: ".len() + metadata.len();
        let command_budget = MAX_COMMAND_BYTES.min(max_body_bytes.saturating_sub(body_overhead));
        let command =
            truncate_middle_to_byte_limit(&format!("{:?}", event.command), command_budget);
        let output_budget = max_body_bytes
            .saturating_sub(body_overhead)
            .saturating_sub(command.len());
        let output = truncate_middle_to_byte_limit(&event.output, output_budget);
        let body = format!("{instruction}\nCommand: {command}{metadata}{output}");

        Some(Self {
            body: truncate_middle_to_byte_limit(&body, max_body_bytes),
        })
    }
}

impl ContextualUserFragment for ProjectValidationFailure {
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (START_MARKER, END_MARKER)
    }

    fn body(&self) -> String {
        let body = &self.body;
        format!("{BODY_PADDING}{body}{BODY_PADDING}")
    }
}

fn truncate_middle_to_byte_limit(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    if max_bytes <= TRUNCATED_MARKER.len() {
        return take_bytes_at_char_boundary(TRUNCATED_MARKER, max_bytes).to_string();
    }
    let content_budget = max_bytes.saturating_sub(TRUNCATED_MARKER.len());
    let prefix = take_bytes_at_char_boundary(value, content_budget / 2);
    let suffix_budget = content_budget.saturating_sub(prefix.len());
    let suffix_start = value
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| value.len().saturating_sub(*index) <= suffix_budget)
        .unwrap_or(value.len());
    format!("{prefix}{TRUNCATED_MARKER}{}", &value[suffix_start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actionable_failure_is_bounded_and_marked() {
        let event = ProjectValidationCompletedEvent {
            turn_id: "turn-1".to_string(),
            command: vec![format!("command-start-{}-command-end", "x".repeat(8_000))],
            cwd: None,
            status: ProjectValidationStatus::ActionableFailure,
            exit_code: Some(7),
            output: "🦀".repeat(4_000),
            output_truncated: true,
            duration_ms: 10,
        };
        let fragment = ProjectValidationFailure::from_event(&event)
            .expect("actionable failure should produce correction context");
        let rendered = fragment.render();

        assert!(rendered.starts_with(START_MARKER));
        assert!(rendered.ends_with(END_MARKER));
        assert!(rendered.contains(TRUNCATED_MARKER));
        assert!(rendered.contains("command-start"));
        assert!(rendered.contains("command-end"));
        assert!(rendered.contains("Exit code: 7"));
        assert!(rendered.contains("Event output truncated: yes"));
        assert!(rendered.len() <= MAX_RENDERED_BYTES);
        assert!(ProjectValidationFailure::matches_text(&rendered));
        assert!(
            ProjectValidationFailure::from_event(&ProjectValidationCompletedEvent {
                status: ProjectValidationStatus::Passed,
                ..event
            })
            .is_none()
        );
    }
}
