//! Native Auto Review status transcript cells.

use super::*;
use codex_app_server_protocol::BackgroundAutoReviewStatus;
use codex_app_server_protocol::BackgroundAutoReviewStatusChangedNotification;
use codex_app_server_protocol::ReviewTarget;

pub(crate) fn new_auto_review_status_cell(
    notification: &BackgroundAutoReviewStatusChangedNotification,
) -> PlainHistoryCell {
    let (symbol, state) = match notification.status {
        BackgroundAutoReviewStatus::Pending => ("○ ".cyan(), "queued".bold()),
        BackgroundAutoReviewStatus::Running => ("○ ".cyan(), "running".bold()),
        BackgroundAutoReviewStatus::Completed => ("✔ ".green(), "completed".bold()),
        BackgroundAutoReviewStatus::Failed => ("✗ ".red(), "failed".bold()),
        BackgroundAutoReviewStatus::Cancelled => ("✗ ".yellow(), "cancelled".bold()),
        BackgroundAutoReviewStatus::Skipped => ("○ ".dim(), "skipped".bold()),
    };
    let mut spans = vec![
        symbol,
        "Auto Review ".into(),
        state,
        " for ".into(),
        Span::from(review_target_label(&notification.review_target)).dim(),
        " · ".dim(),
        Span::from(notification.run_id.clone()).dim(),
    ];
    if let Some(error_summary) = notification
        .error_summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        spans.push(" · ".dim());
        spans.push(Span::from(error_summary.to_string()).red());
    }

    PlainHistoryCell::new(vec![Line::from(spans)])
}

fn review_target_label(target: &ReviewTarget) -> String {
    match target {
        ReviewTarget::UncommittedChanges => "uncommitted changes".to_string(),
        ReviewTarget::BaseBranch { branch } => format!("base branch {branch}"),
        ReviewTarget::Commit { sha, title } => title
            .as_ref()
            .filter(|title| !title.trim().is_empty())
            .map(|title| format!("commit {sha} ({title})"))
            .unwrap_or_else(|| format!("commit {sha}")),
        ReviewTarget::Custom { .. } => "custom review".to_string(),
    }
}
