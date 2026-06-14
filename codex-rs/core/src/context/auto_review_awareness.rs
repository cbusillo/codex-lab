use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use codex_auto_review::AutoReviewFreshness;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunSource;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::AutoReviewSummary;
use codex_auto_review::SUMMARY_MAX_BYTES;
use codex_protocol::protocol::ReviewTarget;

use crate::review_persistence::collect_auto_review_target;
use crate::state::BackgroundAutoReviewActiveSnapshot;

use super::ContextualUserFragment;

const MAX_AWARENESS_BYTES: usize = 4 * 1024;
const MAX_STATUS_LINES: usize = 6;
const MARKER: &str = "... auto review awareness truncated";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutoReviewAwareness {
    body: String,
}

impl AutoReviewAwareness {
    pub(crate) fn new(body: impl Into<String>) -> Option<Self> {
        let body = body.into();
        if body.trim().is_empty() {
            return None;
        }
        Some(Self {
            body: truncate_utf8_with_marker(&body, MAX_AWARENESS_BYTES),
        })
    }
}

impl ContextualUserFragment for AutoReviewAwareness {
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<auto_review_awareness>", "</auto_review_awareness>")
    }

    fn body(&self) -> String {
        let body = &self.body;
        format!("\n{body}\n")
    }
}

pub(crate) async fn build_auto_review_awareness(
    codex_home: &Path,
    cwd: &Path,
    active_snapshot: BackgroundAutoReviewActiveSnapshot,
) -> Option<AutoReviewAwareness> {
    match build_auto_review_awareness_inner(codex_home, cwd, active_snapshot).await {
        Ok(awareness) => awareness,
        Err(err) => {
            tracing::warn!(error = %err, "failed to build auto review awareness context");
            None
        }
    }
}

async fn build_auto_review_awareness_inner(
    codex_home: &Path,
    cwd: &Path,
    active_snapshot: BackgroundAutoReviewActiveSnapshot,
) -> Result<Option<AutoReviewAwareness>> {
    let runs = AutoReviewStore::new(codex_home).list_runs()?;
    if runs.is_empty()
        && active_snapshot.pending_run_id.is_none()
        && active_snapshot.running_run_id.is_none()
    {
        return Ok(None);
    }

    let active_review_target = ReviewTarget::UncommittedChanges;
    let active_target = collect_auto_review_target(cwd, &active_review_target).await;
    Ok(render_awareness(
        &runs,
        &active_target,
        &active_review_target,
        &active_snapshot,
    ))
}

fn render_awareness(
    runs: &[AutoReviewRun],
    active_target: &AutoReviewRunTarget,
    active_review_target: &ReviewTarget,
    active_snapshot: &BackgroundAutoReviewActiveSnapshot,
) -> Option<AutoReviewAwareness> {
    let mut lines = Vec::new();
    let mut status_lines = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut current_summary: Option<(&AutoReviewRun, AutoReviewSummary)> = None;

    if let Some(run_id) = &active_snapshot.pending_run_id {
        status_lines.push(format!("- pending background run: {run_id}"));
    }
    if let Some(run_id) = &active_snapshot.running_run_id {
        status_lines.push(format!("- running background run: {run_id}"));
    }

    for run in runs {
        let freshness = run.freshness(active_target);
        let key = status_count_key(run, freshness, active_review_target);
        *counts.entry(key).or_default() += 1;

        let summary = run.summary(active_target, active_review_target);
        if !summary.content.is_empty() {
            match &current_summary {
                Some((selected_run, _)) if !run_is_newer(run, selected_run) => {}
                _ => current_summary = Some((run, summary)),
            }
        }
    }

    let current_finding_lines = current_summary.map(|(run, summary)| {
        let mut lines = vec![format!(
            "- current findings from run {}: {} rendered, {} omitted",
            run.run_id, summary.rendered_findings, summary.omitted_findings
        )];
        lines.extend(summary.content.lines().map(|line| format!("  {line}")));
        if summary.truncated {
            lines.push("  ... finding summary truncated".to_string());
        }
        lines
    });

    if status_lines.is_empty() && current_finding_lines.is_none() && counts.is_empty() {
        return None;
    }

    lines.push("Auto Review awareness:".to_string());
    lines.push("- target: uncommitted changes".to_string());
    if !status_lines.is_empty() {
        lines.push("- live status:".to_string());
        lines.extend(status_lines.into_iter().take(MAX_STATUS_LINES));
    }
    if let Some(current_finding_lines) = current_finding_lines {
        lines.push("- current finding summaries:".to_string());
        lines.extend(current_finding_lines);
        lines.push(format!(
            "- finding detail is available by stable run_id/finding_id; normal turns include summaries only (max {SUMMARY_MAX_BYTES} bytes)."
        ));
    }
    if !counts.is_empty() {
        lines.push("- recent run status counts:".to_string());
        for (key, count) in counts.into_iter().take(MAX_STATUS_LINES) {
            lines.push(format!("  - {key}: {count}"));
        }
    }

    AutoReviewAwareness::new(lines.join("\n"))
}

fn run_is_newer(candidate: &AutoReviewRun, selected: &AutoReviewRun) -> bool {
    let candidate_time = candidate
        .completed_at_unix_secs
        .unwrap_or(candidate.started_at_unix_secs);
    let selected_time = selected
        .completed_at_unix_secs
        .unwrap_or(selected.started_at_unix_secs);
    (candidate_time, &candidate.run_id) > (selected_time, &selected.run_id)
}

fn status_count_key(
    run: &AutoReviewRun,
    freshness: AutoReviewFreshness,
    active_review_target: &ReviewTarget,
) -> String {
    let freshness = match freshness {
        AutoReviewFreshness::Current => "current",
        AutoReviewFreshness::Stale => "stale",
        AutoReviewFreshness::Detached => "detached",
    };
    let status = match &run.status {
        AutoReviewRunStatus::Pending => "pending",
        AutoReviewRunStatus::Running => "running",
        AutoReviewRunStatus::Completed => "completed",
        AutoReviewRunStatus::Failed => "failed",
        AutoReviewRunStatus::Cancelled => "cancelled",
        AutoReviewRunStatus::Skipped => "skipped",
    };
    let source = match &run.source {
        AutoReviewRunSource::Manual => "manual",
        AutoReviewRunSource::Background => "background",
    };
    let target = if review_target_matches(&run.review_target, active_review_target) {
        "target_match"
    } else {
        "target_mismatch"
    };
    format!("{source}/{status}/{freshness}/{target}")
}

fn review_target_matches(stored: &ReviewTarget, active: &ReviewTarget) -> bool {
    match (stored, active) {
        (
            ReviewTarget::Commit {
                sha: stored_sha, ..
            },
            ReviewTarget::Commit {
                sha: active_sha, ..
            },
        ) => stored_sha == active_sha,
        _ => stored == active,
    }
}

fn truncate_utf8_with_marker(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let marker_budget = MARKER.len().saturating_add(1);
    let max_prefix = max_bytes.saturating_sub(marker_budget);
    let mut end = max_prefix;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n{MARKER}", &value[..end])
}

#[cfg(test)]
#[path = "auto_review_awareness_tests.rs"]
mod tests;
