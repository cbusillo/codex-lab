//! Historical fallbacks for the `commandExecution.aggregatedOutput` field.
//!
//! `aggregated_output` was added to persisted command items after the
//! `stdout`/`stderr`/`formatted_output` fields, so rollouts and thread stores
//! written by older builds only carry the latter. Reads of those items must
//! still surface command output, so the API value falls back through the older
//! fields in the order they were introduced.

/// Resolves the output text published as `aggregatedOutput`, preferring the
/// canonical aggregated field and falling back to the historical ones.
pub(super) fn command_output_text(
    aggregated_output: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
    formatted_output: Option<String>,
) -> Option<String> {
    let combined_output = match (
        stdout.filter(|output| !output.is_empty()),
        stderr.filter(|output| !output.is_empty()),
    ) {
        (Some(stdout), Some(stderr)) => Some(format!("{stdout}{stderr}")),
        (Some(stdout), None) => Some(stdout),
        (None, Some(stderr)) => Some(stderr),
        (None, None) => None,
    };

    [aggregated_output, combined_output, formatted_output]
        .into_iter()
        .flatten()
        .find(|output| !output.is_empty())
}

#[cfg(test)]
#[path = "command_output_tests.rs"]
mod tests;
