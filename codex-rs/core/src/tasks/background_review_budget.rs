use std::sync::Arc;
use std::sync::Mutex;

use codex_auto_review::AutoReviewBudget;
use codex_auto_review::AutoReviewUsage;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use codex_utils_output_truncation::approx_token_count;

use crate::client_common::Prompt;
use crate::context_manager::estimate_item_token_count;

/// Maximum estimation error tolerated after the effective token ceiling.
///
/// Provider usage arrives only after a response completes. Background Review
/// therefore combines a conservative input estimate, a provider-enforced
/// response-output limit, and this reserve so an in-flight response remains
/// within the configured hard ceiling even when accounting is reported late.
const ACCOUNTING_TOLERANCE_DIVISOR: u64 = 25;
const MAX_ACCOUNTING_TOLERANCE_TOKENS: u64 = 10_000;
const TOOL_OUTPUT_BUDGET_DIVISOR: u64 = 32;
const MAX_TOOL_OUTPUT_TOKENS: u64 = 4_096;
/// Below this residual, a reasoning-capable reviewer is unlikely to emit a
/// useful tool call or structured report. Stop with budget remediation instead
/// of sending a request that can only complete as output-limited.
const MIN_USEFUL_RESPONSE_OUTPUT_TOKENS: u64 = 1_024;
/// The shared estimator uses four visible bytes per token. Multiplying its
/// result by four treats each visible byte as a token, providing a conservative
/// upper bound for text and serialized request content.
const INPUT_ESTIMATE_SAFETY_FACTOR: u64 = 4;

#[derive(Clone, Debug)]
pub(crate) struct BackgroundReviewBudgetGate {
    budget: AutoReviewBudget,
    effective_total_token_limit: u64,
    accounting_tolerance_tokens: u64,
    tool_output_limit_tokens: usize,
    state: Arc<Mutex<BackgroundReviewBudgetGateState>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BackgroundReviewBudgetGateState {
    last_observed_total_tokens: u64,
    unaccounted_projected_tokens: u64,
    last_authorized_prompt_tokens: u64,
    projected_total_tokens: Option<u64>,
    request_count: usize,
    retry_count: usize,
    tool_registry_tokens: u64,
    tool_registry_pruned_count: usize,
    tool_output_tokens: u64,
    response_output_limit_tokens: Option<u64>,
    stopped: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PromptTokenEstimate {
    total_tokens: u64,
    tool_registry_tokens: u64,
    tool_output_tokens: u64,
}

impl BackgroundReviewBudgetGate {
    pub(crate) fn new(budget: AutoReviewBudget) -> Self {
        let accounting_tolerance_tokens = budget
            .max_total_tokens
            .div_ceil(ACCOUNTING_TOLERANCE_DIVISOR)
            .clamp(1, MAX_ACCOUNTING_TOLERANCE_TOKENS)
            .min(budget.max_total_tokens.saturating_sub(1));
        let effective_total_token_limit = budget
            .max_total_tokens
            .saturating_sub(accounting_tolerance_tokens)
            .max(1);
        let tool_output_limit_tokens = budget
            .max_total_tokens
            .div_ceil(TOOL_OUTPUT_BUDGET_DIVISOR)
            .clamp(1, MAX_TOOL_OUTPUT_TOKENS) as usize;
        Self {
            budget,
            effective_total_token_limit,
            accounting_tolerance_tokens,
            tool_output_limit_tokens,
            state: Arc::new(Mutex::new(BackgroundReviewBudgetGateState::default())),
        }
    }

    pub(crate) fn tool_output_limit_tokens(&self) -> usize {
        self.tool_output_limit_tokens
    }

    pub(crate) fn authorize_request(
        &self,
        prompt: &mut Prompt,
        token_usage: Option<&TokenUsage>,
        retry: bool,
    ) -> bool {
        let original_tool_count = prompt.tools.len();
        prompt.tools.retain(|tool| review_tool_allowed(tool.name()));
        let pruned_tool_count = original_tool_count.saturating_sub(prompt.tools.len());
        let consumed_tokens = token_usage.and_then(review_token_count).unwrap_or_default();
        let estimate = estimate_prompt_tokens(prompt);
        let mut state = self.state();
        let usage_advanced = consumed_tokens > state.last_observed_total_tokens;
        if usage_advanced {
            state.last_observed_total_tokens = consumed_tokens;
            state.unaccounted_projected_tokens = 0;
            state.last_authorized_prompt_tokens = 0;
        }
        let next_unaccounted_tokens = if retry && !usage_advanced {
            state
                .unaccounted_projected_tokens
                .saturating_sub(state.last_authorized_prompt_tokens)
                .saturating_add(
                    state
                        .last_authorized_prompt_tokens
                        .max(estimate.total_tokens),
                )
        } else {
            state
                .unaccounted_projected_tokens
                .saturating_add(estimate.total_tokens)
        };
        let projected_input_tokens = consumed_tokens.saturating_add(next_unaccounted_tokens);
        let minimum_projected_tokens = projected_input_tokens
            .saturating_add(self.accounting_tolerance_tokens)
            .saturating_add(MIN_USEFUL_RESPONSE_OUTPUT_TOKENS);
        let available_output_tokens = self
            .budget
            .max_total_tokens
            .saturating_sub(projected_input_tokens)
            .saturating_sub(self.accounting_tolerance_tokens);
        let response_output_limit_tokens =
            available_output_tokens.min(prompt.max_output_tokens.unwrap_or(u64::MAX));
        let projected_total_tokens = projected_input_tokens
            .saturating_add(response_output_limit_tokens)
            .saturating_add(self.accounting_tolerance_tokens);
        state.projected_total_tokens = Some(projected_total_tokens);
        state.tool_registry_tokens = state
            .tool_registry_tokens
            .max(estimate.tool_registry_tokens);
        state.tool_registry_pruned_count = state
            .tool_registry_pruned_count
            .saturating_add(pruned_tool_count);
        state.tool_output_tokens = state.tool_output_tokens.max(estimate.tool_output_tokens);
        if minimum_projected_tokens > self.budget.max_total_tokens
            || response_output_limit_tokens < MIN_USEFUL_RESPONSE_OUTPUT_TOKENS
        {
            state.projected_total_tokens = Some(minimum_projected_tokens);
            state.stopped = true;
            return false;
        }
        prompt.max_output_tokens = Some(response_output_limit_tokens);
        state.unaccounted_projected_tokens = next_unaccounted_tokens;
        state.last_authorized_prompt_tokens = estimate.total_tokens;
        state.request_count = state.request_count.saturating_add(1);
        if retry {
            state.retry_count = state.retry_count.saturating_add(1);
        }
        state.response_output_limit_tokens = Some(response_output_limit_tokens);
        true
    }

    pub(crate) fn stop_for_response_output_limit(&self) {
        self.state().stopped = true;
    }

    pub(crate) fn observe_usage(&self, token_usage: Option<&TokenUsage>) -> bool {
        let Some(total_tokens) = token_usage.and_then(review_token_count) else {
            return false;
        };
        let mut state = self.state();
        if total_tokens > state.last_observed_total_tokens {
            state.last_observed_total_tokens = total_tokens;
            state.unaccounted_projected_tokens = 0;
        }
        // The effective limit is a preflight boundary, not a reason to discard
        // a final response that is already completing within the hard ceiling.
        // A subsequent request will include the reserve and be rejected by
        // `authorize_request`; the monitor only interrupts at the hard limit.
        if total_tokens >= self.budget.max_total_tokens {
            state.stopped = true;
        }
        state.stopped
    }

    pub(crate) fn stopped(&self) -> bool {
        self.state().stopped
    }

    pub(crate) fn usage(&self, token_usage: Option<&TokenUsage>) -> AutoReviewUsage {
        let state = *self.state();
        AutoReviewUsage {
            total_tokens: token_usage.and_then(review_token_count),
            input_tokens: token_usage.and_then(|usage| non_negative(usage.input_tokens)),
            cached_input_tokens: token_usage
                .and_then(|usage| non_negative(usage.cached_input_tokens)),
            output_tokens: token_usage.and_then(|usage| non_negative(usage.output_tokens)),
            effective_total_token_limit: Some(self.effective_total_token_limit),
            accounting_tolerance_tokens: Some(self.accounting_tolerance_tokens),
            projected_total_tokens: state.projected_total_tokens,
            request_count: Some(state.request_count),
            retry_count: Some(state.retry_count),
            tool_registry_tokens: Some(state.tool_registry_tokens),
            tool_registry_pruned_count: Some(state.tool_registry_pruned_count),
            tool_output_tokens: Some(state.tool_output_tokens),
            tool_output_limit_tokens: Some(self.tool_output_limit_tokens),
            response_output_limit_tokens: state.response_output_limit_tokens,
            orchestration_skills_suppressed: Some(true),
            ..Default::default()
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, BackgroundReviewBudgetGateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn review_tool_allowed(name: &str) -> bool {
    matches!(name, "shell_command" | "exec_command" | "write_stdin")
}

fn estimate_prompt_tokens(prompt: &Prompt) -> PromptTokenEstimate {
    let input_tokens = prompt
        .input
        .iter()
        .map(estimate_item_token_count)
        .fold(0i64, i64::saturating_add)
        .saturating_add(
            i64::try_from(approx_token_count(&prompt.base_instructions.text)).unwrap_or(i64::MAX),
        );
    let tool_registry_tokens = serde_json::to_string(&prompt.tools)
        .map(|tools| u64::try_from(approx_token_count(&tools)).unwrap_or(u64::MAX))
        .unwrap_or(u64::MAX);
    let tool_output_tokens = prompt
        .input
        .iter()
        .filter(|item| {
            matches!(
                item,
                ResponseItem::FunctionCallOutput { .. }
                    | ResponseItem::CustomToolCallOutput { .. }
                    | ResponseItem::ToolSearchOutput { .. }
            )
        })
        .map(estimate_item_token_count)
        .fold(0i64, i64::saturating_add);
    PromptTokenEstimate {
        total_tokens: u64::try_from(input_tokens.max(0))
            .unwrap_or(u64::MAX)
            .saturating_add(tool_registry_tokens),
        tool_registry_tokens,
        tool_output_tokens: u64::try_from(tool_output_tokens.max(0)).unwrap_or(u64::MAX),
    }
    .with_input_safety_factor()
}

impl PromptTokenEstimate {
    fn with_input_safety_factor(self) -> Self {
        Self {
            total_tokens: self
                .total_tokens
                .saturating_mul(INPUT_ESTIMATE_SAFETY_FACTOR),
            tool_registry_tokens: self.tool_registry_tokens,
            tool_output_tokens: self.tool_output_tokens,
        }
    }
}

fn review_token_count(token_usage: &TokenUsage) -> Option<u64> {
    let reconstructed_total = token_usage
        .non_cached_input()
        .saturating_add(token_usage.cached_input())
        .saturating_add(token_usage.output_tokens.max(0));
    u64::try_from(token_usage.total_tokens.max(reconstructed_total))
        .ok()
        .filter(|total_tokens| *total_tokens > 0)
}

fn non_negative(value: i64) -> Option<u64> {
    u64::try_from(value).ok().filter(|value| *value > 0)
}

#[cfg(test)]
#[path = "background_review_budget_tests.rs"]
mod tests;
