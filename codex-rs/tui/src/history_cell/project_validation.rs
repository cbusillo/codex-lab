//! Automatic Validation disposition transcript cells.

use super::*;
use codex_app_server_protocol::ProjectValidationCompletedNotification;
use codex_app_server_protocol::ProjectValidationSkipReason;
use codex_app_server_protocol::ProjectValidationStatus;

pub(crate) struct ProjectValidationCellData<'a> {
    pub status: ProjectValidationStatus,
    pub skip_reason: Option<ProjectValidationSkipReason>,
    pub changed_file_count: Option<u32>,
    pub command: &'a [String],
    pub command_truncated: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub output_truncated: bool,
}

pub(crate) fn new_project_validation_notification_cell(
    notification: &ProjectValidationCompletedNotification,
) -> PlainHistoryCell {
    new_project_validation_cell(ProjectValidationCellData {
        status: notification.status,
        skip_reason: notification.skip_reason,
        changed_file_count: notification.changed_file_count,
        command: &notification.command,
        command_truncated: notification.command_truncated,
        exit_code: notification.exit_code,
        duration_ms: notification.duration_ms,
        output_truncated: notification.output_truncated,
    })
}

pub(crate) fn new_project_validation_cell(data: ProjectValidationCellData<'_>) -> PlainHistoryCell {
    let ProjectValidationCellData {
        status,
        skip_reason,
        changed_file_count,
        command,
        command_truncated,
        exit_code,
        duration_ms,
        output_truncated,
    } = data;
    let (symbol, state) = match status {
        ProjectValidationStatus::Passed => ("✔ ".green(), "passed".bold()),
        ProjectValidationStatus::ActionableFailure => ("✗ ".red(), "failed".bold()),
        ProjectValidationStatus::ConfigurationError => ("✗ ".red(), "configuration error".bold()),
        ProjectValidationStatus::TimedOut => ("✗ ".red(), "timed out".bold()),
        ProjectValidationStatus::InfrastructureFailure => {
            ("✗ ".red(), "infrastructure failure".bold())
        }
        ProjectValidationStatus::Cancelled => ("○ ".dim(), "cancelled".bold()),
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
        spans.push("command truncated".dim());
    }
    if duration_ms > 0 {
        spans.push(" · ".dim());
        spans.push(Span::from(format!("{duration_ms} ms")).dim());
    }
    if output_truncated {
        spans.push(" · ".dim());
        spans.push("output truncated".dim());
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
