use super::*;
use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use pretty_assertions::assert_eq;

fn budget() -> AutoReviewBudget {
    AutoReviewBudget {
        max_scope_bytes: 1_000,
        max_elapsed_ms: 5_000,
        max_total_tokens: 100,
        max_output_bytes: 64,
        max_findings: 2,
    }
}

fn output_with_findings(count: usize) -> ReviewOutputEvent {
    ReviewOutputEvent {
        findings: (0..count)
            .map(|index| ReviewFinding {
                title: format!("finding {index}"),
                body: String::new(),
                confidence_score: 1.0,
                priority: 1,
                code_location: ReviewCodeLocation {
                    absolute_file_path: std::path::PathBuf::from("/tmp/file.rs"),
                    line_range: ReviewLineRange { start: 1, end: 2 },
                },
            })
            .collect(),
        ..Default::default()
    }
}

/// The monitor loop stops a review as soon as it *reaches* the elapsed or token
/// budget, while the post-run reconciliation only rejects a review that went
/// *over* it. Keeping the two boundaries distinct is what lets a review that
/// finishes exactly on budget still publish its findings.
#[test]
fn elapsed_and_token_budgets_use_reach_then_exceed_boundaries() {
    let budget = budget();

    assert_eq!(
        (
            elapsed_reaches_budget(/*elapsed_ms*/ 4_999, &budget),
            elapsed_reaches_budget(/*elapsed_ms*/ 5_000, &budget),
            elapsed_exceeds_budget(/*elapsed_ms*/ 5_000, &budget),
            elapsed_exceeds_budget(/*elapsed_ms*/ 5_001, &budget),
        ),
        (false, true, false, true)
    );
    assert_eq!(
        (
            total_tokens_reach_budget(/*total_tokens*/ 99, &budget),
            total_tokens_reach_budget(/*total_tokens*/ 100, &budget),
            total_tokens_exceed_budget(/*total_tokens*/ 100, &budget),
            total_tokens_exceed_budget(/*total_tokens*/ 101, &budget),
        ),
        (false, true, false, true)
    );
}

#[test]
fn output_and_finding_budgets_only_trip_when_exceeded() {
    let budget = budget();

    assert_eq!(
        (
            raw_output_exceeds_budget(&"x".repeat(64), &budget),
            raw_output_exceeds_budget(&"x".repeat(65), &budget),
            findings_exceed_budget(&output_with_findings(/*count*/ 2), &budget),
            findings_exceed_budget(&output_with_findings(/*count*/ 3), &budget),
        ),
        (false, true, false, true)
    );
}

/// `total_tokens` is not always populated by the provider, so the reviewer
/// reconstructs it from the component counters and treats zero as "unknown".
#[test]
fn review_token_count_reconstructs_missing_totals_and_ignores_zero() {
    let reconstructed = TokenUsage {
        input_tokens: 30,
        cached_input_tokens: 10,
        output_tokens: 5,
        total_tokens: 0,
        ..Default::default()
    };

    assert_eq!(review_token_count(&reconstructed), Some(35));
    assert_eq!(review_token_count(&TokenUsage::default()), None);
}

#[test]
fn budget_summaries_report_the_limit_that_was_hit() {
    let budget = budget();
    let usage = AutoReviewUsage {
        elapsed_ms: Some(6_000),
        total_tokens: Some(150),
        effective_total_token_limit: Some(96),
        accounting_tolerance_tokens: Some(4),
        projected_total_tokens: Some(180),
        output_bytes: Some(96),
        finding_count: Some(4),
        ..Default::default()
    };

    let summaries = [
        AutoReviewTerminalReason::BudgetElapsed,
        AutoReviewTerminalReason::BudgetTotalTokens,
        AutoReviewTerminalReason::BudgetOutput,
        AutoReviewTerminalReason::BudgetFindingCount,
        AutoReviewTerminalReason::BudgetScope,
    ]
    .map(|reason| background_review_budget_summary(reason, Some(&budget), &usage));

    assert_eq!(
        summaries,
        [
            "background review exceeded elapsed budget: 6000 ms >= 5000 ms; retry after narrowing the review scope or increasing `auto_review.background_max_elapsed_seconds`".to_string(),
            "background review stopped at its effective token budget: consumed 150 tokens, projected 180, effective ceiling 96, hard ceiling 100 (4 token accounting tolerance); retry with a narrower diff; increase `auto_review.background_max_total_tokens` when projected input reached the ceiling, or lower reasoning effort when the response reached its provider cap".to_string(),
            "background review exceeded output budget: 96 bytes > 64 bytes; retry with a narrower diff or increase `auto_review.background_max_output_bytes`".to_string(),
            "background review exceeded finding budget: 4 findings > 2 findings; retry with a narrower diff or increase `auto_review.background_max_findings`".to_string(),
            "background review stopped after exceeding its execution budget".to_string(),
        ]
    );
}

/// Without a resolved budget there is nothing to quote, so the summary must
/// still be a durable, non-empty terminal reason for the persisted run.
#[test]
fn budget_summary_falls_back_when_no_budget_is_attached() {
    assert_eq!(
        background_review_budget_summary(
            AutoReviewTerminalReason::BudgetElapsed,
            /*budget*/ None,
            &AutoReviewUsage::default(),
        ),
        "background review stopped after exceeding its execution budget"
    );
}
