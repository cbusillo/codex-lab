//! Native Background Review status transcript cells.

use super::*;
use codex_app_server_protocol::AutoReviewDiagnosticsSummary;
use codex_app_server_protocol::AutoReviewFindingDisposition;
use codex_app_server_protocol::AutoReviewFreshness;
use codex_app_server_protocol::AutoReviewRunSummary;
use codex_app_server_protocol::AutoReviewStatusCount;
use codex_app_server_protocol::AutoReviewSummaryReadResponse;
use codex_app_server_protocol::AutoReviewTerminalReason;
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
        BackgroundAutoReviewStatus::Cancelled => ("✗ ".dim(), "cancelled".bold()),
        BackgroundAutoReviewStatus::Superseded => ("○ ".dim(), "superseded".bold()),
        BackgroundAutoReviewStatus::Skipped => ("○ ".dim(), "skipped".bold()),
    };
    let mut spans = vec![
        symbol,
        "Background Review ".into(),
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
    push_diagnostics(&mut lines, response.diagnostics.as_ref());
    PlainHistoryCell::new(lines)
}

pub(crate) fn new_auto_review_summary_error_cell(error: String) -> PlainHistoryCell {
    PlainHistoryCell::new(vec![Line::from(vec![
        "✗ ".red(),
        "Background Review summary unavailable".bold(),
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
            "! ".magenta(),
            finding_count_label(summary.rendered_findings),
        ),
        BackgroundAutoReviewStatus::Failed => ("✗ ".red(), "failed".to_string()),
        BackgroundAutoReviewStatus::Cancelled => ("✗ ".dim(), "cancelled".to_string()),
        BackgroundAutoReviewStatus::Superseded => ("○ ".dim(), "superseded".to_string()),
        BackgroundAutoReviewStatus::Skipped => ("○ ".dim(), "skipped".to_string()),
        BackgroundAutoReviewStatus::Pending => ("○ ".cyan(), "queued".to_string()),
        BackgroundAutoReviewStatus::Running => ("○ ".cyan(), "running".to_string()),
    };
    lines.push(Line::from(vec![
        symbol,
        "Background Review ".into(),
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
            "○ ".dim(),
            "Background Review has no current findings".bold(),
            " · latest ".dim(),
            Span::from(freshness_label(latest.freshness)).dim(),
            " · ".dim(),
            Span::from(latest.run_id.clone()).dim(),
        ]));
        push_summary_metadata(lines, latest);
        let hidden_reason = match latest.freshness {
            AutoReviewFreshness::Current => {
                "Latest review output is hidden because it does not apply to this review target."
                    .to_string()
            }
            AutoReviewFreshness::Stale | AutoReviewFreshness::Detached => format!(
                "Latest review output is {} and hidden because it no longer matches this worktree.",
                freshness_label(latest.freshness)
            ),
        };
        lines.push(Line::from(vec![
            "  ".into(),
            Span::from(hidden_reason).dim(),
        ]));
    } else {
        lines.push(Line::from(vec![
            "✔ ".green(),
            "Background Review has no stored result for this thread".bold(),
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
    if let Some(record) = summary.finding_disposition.as_ref() {
        spans.push(" · ".dim());
        spans.push(Span::from(disposition_label(record.disposition)).magenta());
    }
    if let Some(reason) = summary.terminal_reason {
        spans.push(" · ".dim());
        spans.push(Span::from(format!("stopped: {}", terminal_reason_label(reason))).red());
    }
    if let Some(budget) = summary.budget.as_ref() {
        spans.push(" · ".dim());
        spans.push(
            Span::from(format!(
                "elapsed {}/{} · {} · scope {}/{} · output {}/{} · findings {}/{}",
                display_optional_duration(summary.usage.elapsed_ms),
                display_duration(budget.max_elapsed_ms),
                token_budget_label(summary),
                display_optional_bytes(summary.usage.scope_bytes),
                display_bytes(budget.max_scope_bytes),
                display_optional_bytes(summary.usage.output_bytes),
                display_bytes(budget.max_output_bytes),
                summary
                    .usage
                    .finding_count
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "?".to_string()),
                budget.max_findings,
            ))
            .dim(),
        );
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
    push_summary_budget_diagnostics(lines, summary);
}

fn token_budget_label(summary: &AutoReviewRunSummary) -> String {
    let Some(budget) = summary.budget.as_ref() else {
        return String::new();
    };
    let consumed = display_optional_count(summary.usage.total_tokens);
    let hard = display_count(budget.max_total_tokens);
    let Some(effective) = summary.usage.effective_total_token_limit else {
        return format!("tokens {consumed}/{hard}");
    };
    let mut details = vec![format!("{hard} hard")];
    if let Some(tolerance) = summary.usage.accounting_tolerance_tokens {
        details.push(format!("{} reserve", display_count(tolerance)));
    }
    if let Some(projected) = summary.usage.projected_total_tokens {
        details.push(format!("{} projected", display_count(projected)));
    }
    format!(
        "tokens {consumed}/{} effective ({})",
        display_count(effective),
        details.join(", ")
    )
}

fn push_summary_budget_diagnostics(lines: &mut Vec<Line<'static>>, summary: &AutoReviewRunSummary) {
    let usage = &summary.usage;
    let mut details = Vec::new();
    if let Some(request_count) = usage.request_count {
        details.push(format!("requests {request_count}"));
    }
    if let Some(retry_count) = usage.retry_count.filter(|count| *count > 0) {
        details.push(format!("retries {retry_count}"));
    }
    if let Some(tokens) = usage.tool_registry_tokens {
        details.push(format!("registry {} tokens", display_count(tokens)));
    }
    if let Some(pruned_count) = usage.tool_registry_pruned_count.filter(|count| *count > 0) {
        details.push(format!("registry tools pruned {pruned_count}"));
    }
    if let Some(tokens) = usage.tool_output_tokens {
        let limit = usage
            .tool_output_limit_tokens
            .and_then(|limit| u64::try_from(limit).ok())
            .map(display_count)
            .unwrap_or_else(|| "?".to_string());
        details.push(format!(
            "tool output {}/{} tokens",
            display_count(tokens),
            limit
        ));
    }
    if let Some(tokens) = usage.response_output_limit_tokens {
        details.push(format!("response cap {} tokens", display_count(tokens)));
    }
    if usage.orchestration_skills_suppressed == Some(true) {
        details.push("orchestration guidance suppressed".to_string());
    }
    if !details.is_empty() {
        lines.push(Line::from(vec![
            "  ".into(),
            Span::from(details.join(" · ")).dim(),
        ]));
    }
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

fn push_diagnostics(
    lines: &mut Vec<Line<'static>>,
    diagnostics: Option<&AutoReviewDiagnosticsSummary>,
) {
    let Some(diagnostics) = diagnostics else {
        return;
    };
    let compact = diagnostics.compact.trim();
    if compact.is_empty() {
        return;
    }
    lines.push(Line::from(vec![
        "  ".into(),
        "diagnostics ".dim(),
        Span::from(compact.to_string()).dim(),
    ]));
}

fn finding_count_label(count: usize) -> String {
    match count {
        0 => "found no findings".to_string(),
        1 => "found 1 finding".to_string(),
        count => format!("found {count} findings"),
    }
}

fn disposition_label(disposition: AutoReviewFindingDisposition) -> &'static str {
    match disposition {
        AutoReviewFindingDisposition::NeedsAttention => "needs attention",
        AutoReviewFindingDisposition::Repairing => "repairing",
        AutoReviewFindingDisposition::Deferred => "deferred",
        AutoReviewFindingDisposition::Obsolete => "obsolete",
    }
}

fn terminal_reason_label(reason: AutoReviewTerminalReason) -> &'static str {
    match reason {
        AutoReviewTerminalReason::BudgetScope => "scope budget",
        AutoReviewTerminalReason::BudgetElapsed => "elapsed budget",
        AutoReviewTerminalReason::BudgetTotalTokens => "token budget",
        AutoReviewTerminalReason::BudgetOutput => "output budget",
        AutoReviewTerminalReason::BudgetFindingCount => "finding budget",
        AutoReviewTerminalReason::EmptyOutput => "empty output",
        AutoReviewTerminalReason::StaleTarget => "stale target",
    }
}

fn display_optional_duration(milliseconds: Option<u64>) -> String {
    milliseconds
        .map(display_duration)
        .unwrap_or_else(|| "?".to_string())
}

fn display_duration(milliseconds: u64) -> String {
    if milliseconds >= 60_000 {
        format!("{}m", milliseconds / 60_000)
    } else if milliseconds >= 1_000 {
        format!("{}s", milliseconds / 1_000)
    } else {
        format!("{milliseconds}ms")
    }
}

fn display_optional_count(count: Option<u64>) -> String {
    count.map(display_count).unwrap_or_else(|| "?".to_string())
}

fn display_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{}m", count / 1_000_000)
    } else if count >= 1_000 {
        format!("{}k", count / 1_000)
    } else {
        count.to_string()
    }
}

fn display_optional_bytes(bytes: Option<usize>) -> String {
    bytes.map(display_bytes).unwrap_or_else(|| "?".to_string())
}

fn display_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{}MiB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{}KiB", bytes / 1024)
    } else {
        format!("{bytes}B")
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
        BackgroundAutoReviewStatus::Superseded => "superseded",
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
