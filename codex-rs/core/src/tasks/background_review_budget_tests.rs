use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::ToolSpec;

use crate::config::DEFAULT_BACKGROUND_AUTO_REVIEW_MAX_DIFF_BYTES;
use crate::config::DEFAULT_BACKGROUND_AUTO_REVIEW_MAX_TOTAL_TOKENS;

fn budget(max_total_tokens: u64) -> AutoReviewBudget {
    AutoReviewBudget {
        max_scope_bytes: 1_000_000,
        max_elapsed_ms: 60_000,
        max_total_tokens,
        max_output_bytes: 64_000,
        max_findings: 10,
    }
}

fn prompt(text: &str) -> Prompt {
    Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        base_instructions: codex_protocol::models::BaseInstructions {
            text: "review directly".to_string(),
        },
        ..Default::default()
    }
}

fn budget_with_output_bytes(max_total_tokens: u64, max_output_bytes: usize) -> AutoReviewBudget {
    AutoReviewBudget {
        max_output_bytes,
        ..budget(max_total_tokens)
    }
}

#[test]
fn reserves_bounded_headroom_and_caps_each_tool_output() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 250_000));
    let usage = gate.usage(/*token_usage*/ None);

    assert_eq!(usage.effective_total_token_limit, Some(240_000));
    assert_eq!(usage.accounting_tolerance_tokens, Some(10_000));
    assert_eq!(usage.tool_output_limit_tokens, Some(4_096));
    assert_eq!(
        usage.response_output_reservation_tokens,
        Some(INTRINSIC_MODEL_OUTPUT_RESERVATION_TOKENS)
    );
    assert_eq!(usage.orchestration_skills_suppressed, Some(true));
}

#[test]
fn default_output_bytes_bound_the_provider_response() {
    let gate = BackgroundReviewBudgetGate::new(budget_with_output_bytes(
        /*max_total_tokens*/ 250_000, /*max_output_bytes*/ 65_536,
    ));
    let mut request = prompt("inspect the diff");

    assert!(gate.authorize_request(&mut request, None, /*retry*/ false));
    assert_eq!(
        request.max_output_tokens,
        Some(MAX_REQUESTED_RESPONSE_OUTPUT_TOKENS)
    );
    assert_eq!(
        gate.usage(None).response_output_limit_tokens,
        Some(MAX_REQUESTED_RESPONSE_OUTPUT_TOKENS)
    );
}

#[test]
fn intrinsic_output_reservation_drives_the_projected_total() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 250_000));
    let mut request = prompt("inspect the diff");

    assert!(gate.authorize_request(&mut request, None, /*retry*/ false));
    let usage = gate.usage(None);
    assert_eq!(
        usage.response_output_reservation_tokens,
        Some(INTRINSIC_MODEL_OUTPUT_RESERVATION_TOKENS)
    );
    assert!(
        usage
            .projected_total_tokens
            .is_some_and(|tokens| tokens > 138_000 && tokens <= 250_000)
    );
}

#[test]
fn intrinsic_output_reservation_rejects_an_unsound_total_budget() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 100_000));
    let mut request = prompt("inspect the diff");

    assert!(!gate.authorize_request(&mut request, None, /*retry*/ false));
    assert!(gate.stopped());
    assert!(
        gate.usage(None)
            .projected_total_tokens
            .is_some_and(|tokens| tokens > 100_000)
    );
}

#[test]
fn default_budget_authorizes_the_maximum_default_scope() {
    let gate = BackgroundReviewBudgetGate::new(AutoReviewBudget {
        max_total_tokens: DEFAULT_BACKGROUND_AUTO_REVIEW_MAX_TOTAL_TOKENS,
        ..budget(DEFAULT_BACKGROUND_AUTO_REVIEW_MAX_TOTAL_TOKENS)
    });
    let mut request = prompt(&"x".repeat(DEFAULT_BACKGROUND_AUTO_REVIEW_MAX_DIFF_BYTES));

    assert!(gate.authorize_request(&mut request, None, /*retry*/ false));
    assert!(
        gate.usage(None)
            .projected_total_tokens
            .is_some_and(|tokens| tokens <= DEFAULT_BACKGROUND_AUTO_REVIEW_MAX_TOTAL_TOKENS)
    );
}

#[test]
fn smaller_output_budget_tightens_the_provider_response() {
    let gate = BackgroundReviewBudgetGate::new(budget_with_output_bytes(
        /*max_total_tokens*/ 250_000, /*max_output_bytes*/ 4_096,
    ));
    let mut request = prompt("inspect the diff");

    assert!(gate.authorize_request(&mut request, None, /*retry*/ false));
    assert_eq!(request.max_output_tokens, Some(4_096));
}

#[test]
fn output_budget_below_a_useful_response_preserves_the_existing_output_check() {
    let gate = BackgroundReviewBudgetGate::new(budget_with_output_bytes(
        /*max_total_tokens*/ 250_000, /*max_output_bytes*/ 4,
    ));
    let mut request = prompt("inspect the diff");

    assert!(gate.authorize_request(&mut request, None, /*retry*/ false));
    assert_eq!(
        request.max_output_tokens,
        Some(MIN_USEFUL_RESPONSE_OUTPUT_TOKENS)
    );
}

#[test]
fn one_token_budget_has_no_impossible_reserve() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 1));
    let usage = gate.usage(/*token_usage*/ None);

    assert_eq!(usage.effective_total_token_limit, Some(1));
    assert_eq!(usage.accounting_tolerance_tokens, Some(0));
}

#[test]
fn completed_usage_inside_hard_ceiling_is_not_stopped_by_effective_reserve() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 1_000));
    let token_usage = TokenUsage {
        input_tokens: 970,
        total_tokens: 970,
        ..Default::default()
    };

    assert!(!gate.observe_usage(Some(&token_usage)));
}

#[test]
fn rejects_request_when_consumed_and_projected_growth_cross_hard_ceiling() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 1_000));
    let token_usage = TokenUsage {
        input_tokens: 930,
        total_tokens: 930,
        ..Default::default()
    };

    let mut request = prompt(&"x".repeat(200));
    assert!(!gate.authorize_request(&mut request, Some(&token_usage), /*retry*/ false));
    assert!(gate.stopped());
    let usage = gate.usage(Some(&token_usage));
    assert_eq!(usage.total_tokens, Some(930));
    assert!(
        usage
            .projected_total_tokens
            .is_some_and(|tokens| tokens > 1_000)
    );
    assert_eq!(usage.request_count, Some(0));
}

#[test]
fn unreported_retry_replaces_the_previous_projection_instead_of_stacking_it() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 250_000));
    let mut request = prompt("inspect the diff");

    assert!(gate.authorize_request(
        &mut request,
        /*token_usage*/ None,
        /*retry*/ false,
    ));
    let first_projection = gate.usage(None).projected_total_tokens;
    assert!(gate.authorize_request(&mut request, /*token_usage*/ None, /*retry*/ true,));
    let usage = gate.usage(None);
    assert_eq!(usage.projected_total_tokens, first_projection);
    assert_eq!(gate.usage(None).request_count, Some(2));
    assert_eq!(gate.usage(None).retry_count, Some(1));
}

#[test]
fn cumulative_usage_resets_projection_across_multiple_requests() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 250_000));
    let mut request = prompt("inspect the diff");

    assert!(gate.authorize_request(&mut request, None, /*retry*/ false));
    assert!(!gate.observe_usage(Some(&TokenUsage {
        input_tokens: 1_000,
        total_tokens: 1_000,
        ..Default::default()
    })));
    assert!(gate.authorize_request(
        &mut request,
        Some(&TokenUsage {
            input_tokens: 1_800,
            output_tokens: 200,
            total_tokens: 2_000,
            ..Default::default()
        }),
        /*retry*/ false,
    ));
    assert!(!gate.observe_usage(Some(&TokenUsage {
        input_tokens: 2_700,
        output_tokens: 300,
        total_tokens: 3_000,
        ..Default::default()
    })));
    assert!(gate.authorize_request(
        &mut request,
        Some(&TokenUsage {
            input_tokens: 2_700,
            output_tokens: 300,
            total_tokens: 3_000,
            ..Default::default()
        }),
        /*retry*/ false,
    ));

    let usage = gate.usage(Some(&TokenUsage {
        input_tokens: 2_700,
        output_tokens: 300,
        total_tokens: 3_000,
        ..Default::default()
    }));
    assert_eq!(usage.request_count, Some(3));
    assert_eq!(usage.retry_count, Some(0));
    assert!(
        usage
            .projected_total_tokens
            .is_some_and(|tokens| tokens <= 250_000)
    );
    assert!(!gate.stopped());
}

#[test]
fn prunes_non_review_tools_before_estimating_the_registry() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 250_000));
    let mut request = prompt("inspect the diff");
    request.tools = vec![
        freeform_tool("exec_command"),
        freeform_tool("mcp__large__lookup"),
    ];

    assert!(gate.authorize_request(&mut request, None, /*retry*/ false));

    assert_eq!(
        request.tools.iter().map(ToolSpec::name).collect::<Vec<_>>(),
        vec!["exec_command"]
    );
    assert_eq!(gate.usage(None).tool_registry_pruned_count, Some(1));
}

#[test]
fn caps_provider_output_inside_the_hard_ceiling() {
    let gate = BackgroundReviewBudgetGate::new(budget(/*max_total_tokens*/ 250_000));
    let mut request = prompt("inspect the diff");

    assert!(gate.authorize_request(&mut request, None, /*retry*/ false));

    let usage = gate.usage(None);
    let limit = request.max_output_tokens.expect("provider output limit");
    assert_eq!(usage.response_output_limit_tokens, Some(limit));
    assert!(limit > 0);
    assert!(
        usage
            .projected_total_tokens
            .is_some_and(|tokens| tokens <= 250_000)
    );
}

fn freeform_tool(name: &str) -> ToolSpec {
    ToolSpec::Freeform(FreeformTool {
        name: name.to_string(),
        description: "test tool".to_string(),
        format: FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: "lark".to_string(),
            definition: "start: /.+/".to_string(),
        },
    })
}
