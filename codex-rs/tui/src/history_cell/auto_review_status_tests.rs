use super::*;
use codex_app_server_protocol::AutoReviewFreshness;
use codex_app_server_protocol::AutoReviewRunSource;
use codex_app_server_protocol::AutoReviewStatusCount;
use codex_app_server_protocol::AutoReviewUsage;

#[test]
fn hidden_auto_review_summaries_explain_stale_and_detached_results() {
    let rendered = [
        AutoReviewFreshness::Current,
        AutoReviewFreshness::Stale,
        AutoReviewFreshness::Detached,
    ]
    .into_iter()
    .map(|freshness| {
        let cell = new_hidden_auto_review_run_summary_cell(
            &run_summary(freshness),
            &[status_count(freshness)],
            /*diagnostics*/ None,
        );
        render_lines(&cell.display_lines(/*width*/ 100)).join("\n")
    })
    .collect::<Vec<_>>()
    .join("\n---\n");

    insta::assert_snapshot!(rendered);
}

fn status_count(freshness: AutoReviewFreshness) -> AutoReviewStatusCount {
    AutoReviewStatusCount {
        status: BackgroundAutoReviewStatus::Completed,
        source: AutoReviewRunSource::Background,
        freshness,
        target_matches: false,
        count: 2,
    }
}

fn run_summary(freshness: AutoReviewFreshness) -> AutoReviewRunSummary {
    AutoReviewRunSummary {
        run_id: format!("run-{freshness:?}").to_lowercase(),
        status: BackgroundAutoReviewStatus::Completed,
        source: AutoReviewRunSource::Background,
        freshness,
        started_at: 1_700_000_000,
        completed_at: Some(1_700_000_060),
        model: Some("review-model".to_string()),
        error_summary: None,
        rendered_findings: 2,
        omitted_findings: 0,
        truncated: false,
        content: "unused hidden findings".to_string(),
        budget: None,
        usage: AutoReviewUsage::default(),
        terminal_reason: None,
        finding_disposition: None,
    }
}

fn render_lines(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}
