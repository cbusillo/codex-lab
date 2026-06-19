//! Native Auto Review status transcript cells.

use super::*;
use codex_app_server_protocol::AutoReviewFreshness;
use codex_app_server_protocol::AutoReviewRunSummary;
use codex_app_server_protocol::AutoReviewStatusCount;
use codex_app_server_protocol::AutoReviewSummaryReadResponse;
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

pub(crate) fn new_auto_review_summary_cell(
    response: &AutoReviewSummaryReadResponse,
) -> PlainHistoryCell {
    let mut lines = Vec::new();
    match response.current.as_ref() {
        Some(summary) => push_current_summary_lines(&mut lines, summary),
        None => push_no_current_summary_lines(&mut lines, response),
    }
    PlainHistoryCell::new(lines)
}

pub(crate) fn new_auto_review_summary_error_cell(error: String) -> PlainHistoryCell {
    PlainHistoryCell::new(vec![Line::from(vec![
        "✗ ".red(),
        "Auto Review summary unavailable".bold(),
        " · ".dim(),
        error.dim(),
    ])])
}

fn push_current_summary_lines(lines: &mut Vec<Line<'static>>, summary: &AutoReviewRunSummary) {
    let (symbol, headline) = match summary.status {
        BackgroundAutoReviewStatus::Completed if summary.rendered_findings == 0 => {
            ("✔ ".green(), finding_count_label(summary.rendered_findings))
        }
        BackgroundAutoReviewStatus::Completed => (
            "! ".yellow(),
            finding_count_label(summary.rendered_findings),
        ),
        BackgroundAutoReviewStatus::Failed => ("✗ ".red(), "failed".to_string()),
        BackgroundAutoReviewStatus::Cancelled => ("✗ ".yellow(), "cancelled".to_string()),
        BackgroundAutoReviewStatus::Skipped => ("○ ".dim(), "skipped".to_string()),
        BackgroundAutoReviewStatus::Pending => ("○ ".cyan(), "queued".to_string()),
        BackgroundAutoReviewStatus::Running => ("○ ".cyan(), "running".to_string()),
    };
    lines.push(Line::from(vec![
        symbol,
        "Auto Review ".into(),
        headline.bold(),
        " · ".dim(),
        Span::from(summary.run_id.clone()).dim(),
    ]));
    push_summary_metadata(lines, summary);
    push_summary_content(lines, summary);
}

fn push_no_current_summary_lines(
    lines: &mut Vec<Line<'static>>,
    response: &AutoReviewSummaryReadResponse,
) {
    if let Some(latest) = response.latest.as_ref() {
        lines.push(Line::from(vec![
            "○ ".yellow(),
            "Auto Review has no current findings".bold(),
            " · latest ".dim(),
            Span::from(freshness_label(latest.freshness)).yellow(),
            " · ".dim(),
            Span::from(latest.run_id.clone()).dim(),
        ]));
        push_summary_metadata(lines, latest);
        if latest.content.trim().is_empty() {
            lines.push(Line::from(vec![
                "  ".into(),
                "Latest findings are hidden because they no longer match this worktree.".dim(),
            ]));
        } else {
            push_summary_content(lines, latest);
        }
    } else {
        lines.push(Line::from(vec![
            "✔ ".green(),
            "Auto Review has no stored result for this thread".bold(),
        ]));
    }
    push_status_counts(lines, &response.status_counts);
}

fn push_summary_metadata(lines: &mut Vec<Line<'static>>, summary: &AutoReviewRunSummary) {
    let mut spans = vec![
        "  ".into(),
        Span::from(status_label(summary.status)).dim(),
        " · ".dim(),
        Span::from(freshness_label(summary.freshness)).dim(),
    ];
    if let Some(model) = summary
        .model
        .as_deref()
        .filter(|model| !model.trim().is_empty())
    {
        spans.push(" · ".dim());
        spans.push(Span::from(model.to_string()).dim());
    }
    if summary.omitted_findings > 0 {
        spans.push(" · ".dim());
        spans.push(Span::from(format!("{} omitted", summary.omitted_findings)).dim());
    }
    if summary.truncated {
        spans.push(" · truncated".dim());
    }
    if let Some(error_summary) = summary
        .error_summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        spans.push(" · ".dim());
        spans.push(Span::from(error_summary.to_string()).red());
    }
    lines.push(Line::from(spans));
}

fn push_summary_content(lines: &mut Vec<Line<'static>>, summary: &AutoReviewRunSummary) {
    let content = summary.content.trim();
    if content.is_empty() {
        if summary.status == BackgroundAutoReviewStatus::Completed && summary.rendered_findings == 0
        {
            lines.push(Line::from(vec!["  ".into(), "No findings.".dim()]));
        }
        return;
    }
    lines.extend(prefix_lines(
        raw_lines_from_source(content),
        "  ".into(),
        "  ".into(),
    ));
}

fn push_status_counts(lines: &mut Vec<Line<'static>>, counts: &[AutoReviewStatusCount]) {
    if counts.is_empty() {
        return;
    }
    let labels = counts
        .iter()
        .map(|count| {
            format!(
                "{} {} {}{}",
                count.count,
                freshness_label(count.freshness),
                status_label(count.status),
                if count.target_matches {
                    ""
                } else {
                    " off-target"
                }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(Line::from(vec!["  ".into(), labels.dim()]));
}

fn finding_count_label(count: usize) -> String {
    match count {
        0 => "found no findings".to_string(),
        1 => "found 1 finding".to_string(),
        count => format!("found {count} findings"),
    }
}

fn freshness_label(freshness: AutoReviewFreshness) -> &'static str {
    match freshness {
        AutoReviewFreshness::Current => "current",
        AutoReviewFreshness::Stale => "stale",
        AutoReviewFreshness::Detached => "detached",
    }
}

fn status_label(status: BackgroundAutoReviewStatus) -> &'static str {
    match status {
        BackgroundAutoReviewStatus::Pending => "queued",
        BackgroundAutoReviewStatus::Running => "running",
        BackgroundAutoReviewStatus::Completed => "completed",
        BackgroundAutoReviewStatus::Failed => "failed",
        BackgroundAutoReviewStatus::Cancelled => "cancelled",
        BackgroundAutoReviewStatus::Skipped => "skipped",
    }
}

fn review_target_label(target: &ReviewTarget) -> String {
    match target {
        ReviewTarget::UncommittedChanges => "uncommitted changes".to_string(),
        ReviewTarget::CurrentTurnDiff { .. } => "current turn changes".to_string(),
        ReviewTarget::BaseBranch { branch } => format!("base branch {branch}"),
        ReviewTarget::Commit { sha, title } => title
            .as_ref()
            .filter(|title| !title.trim().is_empty())
            .map(|title| format!("commit {sha} ({title})"))
            .unwrap_or_else(|| format!("commit {sha}")),
        ReviewTarget::Custom { .. } => "custom review".to_string(),
    }
}
