use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use codex_auto_review::AutoReviewFindingDisposition;
use codex_auto_review::AutoReviewLedgerProjection;
use codex_auto_review::AutoReviewRun;
use codex_auto_review::AutoReviewRunState;
use codex_auto_review::AutoReviewRunStatus;
use codex_auto_review::AutoReviewRunTarget;
use codex_auto_review::AutoReviewStore;
use codex_auto_review::AutoReviewSummary;
use codex_auto_review::SUMMARY_MAX_BYTES;
use codex_protocol::protocol::ReviewTarget;

use crate::review_persistence::collect_auto_review_target;
use crate::state::BackgroundAutoReviewActiveSnapshot;

use super::ContextualUserFragment;

const AWARENESS_START_MARKER: &str = "<auto_review_awareness>";
const AWARENESS_END_MARKER: &str = "</auto_review_awareness>";
/// Manually reviewed hard cap on the fully rendered fragment (both markers plus the body
/// wrapper). 2 KiB of this ASCII-dominated status text is well under the 1K-token ceiling that
/// `.codex/skills/code-review-context` requires for a per-turn injected fragment.
const MAX_RENDERED_AWARENESS_BYTES: usize = 2 * 1024;
/// `body()` wraps the stored body in a leading and trailing newline.
const AWARENESS_BODY_WRAPPER_BYTES: usize = 2;
const MAX_AWARENESS_BYTES: usize = MAX_RENDERED_AWARENESS_BYTES
    - AWARENESS_START_MARKER.len()
    - AWARENESS_END_MARKER.len()
    - AWARENESS_BODY_WRAPPER_BYTES;
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
        (AWARENESS_START_MARKER, AWARENESS_END_MARKER)
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
    if active_snapshot.pending_run_id.is_none()
        && active_snapshot.running_run_id.is_none()
        && !AutoReviewStore::has_store_files(codex_home)
    {
        return Ok(None);
    }

    let active_review_target = ReviewTarget::UncommittedChanges;
    let active_target = collect_auto_review_target(codex_home, cwd, &active_review_target).await;
    let store_scope = active_target.worktree_path.as_deref().unwrap_or(cwd);
    let store = AutoReviewStore::for_scope(codex_home, store_scope);
    let runs = store.list_runs()?;
    if runs.is_empty()
        && active_snapshot.pending_run_id.is_none()
        && active_snapshot.running_run_id.is_none()
    {
        return Ok(None);
    }

    let run_states = runs
        .iter()
        .filter(|run| {
            run.finding_count > 0 && run.can_read_detail(&active_target, &active_review_target)
        })
        .filter_map(|run| {
            store
                .load_run_state(&run.run_id)
                .ok()
                .flatten()
                .map(|state| (run.run_id.clone(), state))
        })
        .collect();
    Ok(render_awareness_with_states(
        &runs,
        &run_states,
        &active_target,
        &active_review_target,
        &active_snapshot,
    ))
}

#[cfg(test)]
fn render_awareness(
    runs: &[AutoReviewRun],
    active_target: &AutoReviewRunTarget,
    active_review_target: &ReviewTarget,
    active_snapshot: &BackgroundAutoReviewActiveSnapshot,
) -> Option<AutoReviewAwareness> {
    render_awareness_with_states(
        runs,
        &BTreeMap::new(),
        active_target,
        active_review_target,
        active_snapshot,
    )
}

fn render_awareness_with_states(
    runs: &[AutoReviewRun],
    run_states: &BTreeMap<String, AutoReviewRunState>,
    active_target: &AutoReviewRunTarget,
    active_review_target: &ReviewTarget,
    active_snapshot: &BackgroundAutoReviewActiveSnapshot,
) -> Option<AutoReviewAwareness> {
    let mut lines = Vec::new();
    let mut status_lines = Vec::new();
    let mut current_summary: Option<(&AutoReviewRun, AutoReviewSummary)> = None;
    let visible_runs = runs
        .iter()
        .filter(|run| run_is_visible_for_awareness(run, active_review_target))
        .cloned()
        .collect::<Vec<_>>();
    let projection =
        AutoReviewLedgerProjection::from_runs(&visible_runs, active_target, active_review_target);

    if let Some(run_id) = &active_snapshot.pending_run_id {
        status_lines.push(format!("- pending background run: {run_id}"));
    }
    if let Some(run_id) = &active_snapshot.running_run_id {
        status_lines.push(format!("- running background run: {run_id}"));
    }

    for run in &visible_runs {
        if !run_has_actionable_disposition(run, run_states.get(&run.run_id)) {
            continue;
        }
        let summary = run.project(active_target, active_review_target).summary;
        if !summary.content.is_empty() {
            match &current_summary {
                Some((selected_run, _)) if run.sort_key() <= selected_run.sort_key() => {}
                _ => current_summary = Some((run, summary)),
            }
        }
    }

    let current_finding_lines = current_summary.map(|(run, summary)| {
        let disposition = run_states
            .get(&run.run_id)
            .and_then(|state| state.finding_disposition.as_ref())
            .map(|record| match record.disposition {
                AutoReviewFindingDisposition::NeedsAttention => "needs_attention",
                AutoReviewFindingDisposition::Repairing => "repairing",
                AutoReviewFindingDisposition::Deferred => "deferred",
                AutoReviewFindingDisposition::Obsolete => "obsolete",
            })
            .unwrap_or("needs_attention");
        let mut lines = vec![format!(
            "- current findings from run {}: disposition={disposition}, {} rendered, {} omitted",
            run.run_id, summary.rendered_findings, summary.omitted_findings,
        )];
        lines.extend(summary.content.lines().map(|line| format!("  {line}")));
        if summary.truncated {
            lines.push("  ... finding summary truncated".to_string());
        }
        lines
    });

    let has_diagnostic_state = visible_runs.iter().any(|run| {
        !matches!(run.status, AutoReviewRunStatus::Completed)
            || (run.finding_count > 0
                && run_has_actionable_disposition(run, run_states.get(&run.run_id)))
    });
    if status_lines.is_empty() && current_finding_lines.is_none() && !has_diagnostic_state {
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
        lines.push(
            "- disposition required: use auto_review_disposition with repair, defer, or obsolete (obsolete requires a reason)."
                .to_string(),
        );
    }
    if has_diagnostic_state && !projection.status_counts.is_empty() {
        lines.push("- recent run status counts:".to_string());
        for count in projection.status_counts.iter().take(MAX_STATUS_LINES) {
            lines.push(format!("  - {}: {}", count.label(), count.count));
        }
    }
    if has_diagnostic_state && let Some(diagnostics) = projection.diagnostics {
        lines.push(format!(
            "- recent run diagnostics: {}",
            diagnostics.compact_line()
        ));
    }

    AutoReviewAwareness::new(lines.join("\n"))
}

fn run_has_actionable_disposition(run: &AutoReviewRun, state: Option<&AutoReviewRunState>) -> bool {
    run.finding_count > 0
        && state
            .and_then(|state| state.finding_disposition.as_ref())
            .is_none_or(|record| {
                matches!(
                    record.disposition,
                    AutoReviewFindingDisposition::NeedsAttention
                        | AutoReviewFindingDisposition::Repairing
                )
            })
}

fn run_is_visible_for_awareness(run: &AutoReviewRun, active_review_target: &ReviewTarget) -> bool {
    !matches!(
        (&run.review_target, active_review_target),
        (
            ReviewTarget::CurrentTurnDiff { .. },
            ReviewTarget::UncommittedChanges
        )
    ) || run.target.worktree_diff_fingerprint.is_some()
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
