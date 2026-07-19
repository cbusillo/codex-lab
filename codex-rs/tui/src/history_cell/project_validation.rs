//! Automatic Validation disposition transcript cells.

use super::*;
use codex_app_server_protocol::ProjectValidationCompletedNotification;
use codex_app_server_protocol::ProjectValidationSkipReason;
use codex_app_server_protocol::ProjectValidationStatus;

pub(crate) fn new_project_validation_notification_cell(
    notification: &ProjectValidationCompletedNotification,
) -> PlainHistoryCell {
    new_project_validation_cell(
        notification.status,
        notification.skip_reason,
        notification.changed_file_count,
        &notification.command,
        notification.command_truncated,
        notification.exit_code,
        notification.duration_ms,
        notification.output_truncated,
    )
}

pub(crate) fn new_project_validation_cell(
    status: ProjectValidationStatus,
    skip_reason: Option<ProjectValidationSkipReason>,
    changed_file_count: Option<u32>,
    command: &[String],
    command_truncated: bool,
    exit_code: Option<i32>,
    duration_ms: u64,
    output_truncated: bool,
) -> PlainHistoryCell {
    let (symbol, state) = match status {
        ProjectValidationStatus::Passed => ("✔ ".green(), "passed".bold()),
        ProjectValidationStatus::ActionableFailure => ("✗ ".red(), "failed".bold()),
        ProjectValidationStatus::ConfigurationError => ("✗ ".red(), "configuration error".bold()),
        ProjectValidationStatus::TimedOut => ("✗ ".yellow(), "timed out".bold()),
        ProjectValidationStatus::InfrastructureFailure => {
            ("✗ ".red(), "infrastructure failure".bold())
        }
        ProjectValidationStatus::Cancelled => ("○ ".yellow(), "cancelled".bold()),
        ProjectValidationStatus::Skipped => ("○ ".dim(), "skipped".bold()),
    };
    let mut spans = vec![symbol, "Automatic Validation ".into(), state];
    if let Some(reason) = skip_reason {
        spans.push(" · ".dim());
        spans.push(Span::from(skip_reason_label(reason)).dim());
    }
    if let Some(count) = changed_file_count {
        spans.push(" · ".dim());
        spans.push(Span::from(changed_file_count_label(count)).dim());
    }
    if status == ProjectValidationStatus::ActionableFailure {
        spans.push(" · ".dim());
        spans.push(Span::from(format!("exit {}", exit_code.unwrap_or(1))).red());
    }
    if !command.is_empty() {
        spans.push(" · ".dim());
        spans.push(Span::from(command.join(" ")).dim());
    }
    if command_truncated {
        spans.push(" · ".dim());
        spans.push("command truncated".yellow());
    }
    if duration_ms > 0 {
        spans.push(" · ".dim());
        spans.push(Span::from(format!("{duration_ms} ms")).dim());
    }
    if output_truncated {
        spans.push(" · ".dim());
        spans.push("output truncated".yellow());
    }
    PlainHistoryCell::new(vec![Line::from(spans)])
}

fn skip_reason_label(reason: ProjectValidationSkipReason) -> &'static str {
    match reason {
        ProjectValidationSkipReason::ValidationDisabled => "validation disabled",
        ProjectValidationSkipReason::NoChangedFiles => "no changed files",
        ProjectValidationSkipReason::NoApplicableProvider => "no applicable provider",
        ProjectValidationSkipReason::NonRootAgent => "non-root agent",
        ProjectValidationSkipReason::UnchangedFingerprint => "unchanged worktree",
        ProjectValidationSkipReason::UnsupportedEnvironment => "unsupported environment",
    }
}

fn changed_file_count_label(count: u32) -> String {
    let suffix = if count == 1 { "file" } else { "files" };
    format!("{count} changed {suffix}")
}
