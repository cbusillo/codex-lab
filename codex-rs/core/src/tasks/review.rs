use std::sync::Arc;
use std::time::Duration;

use codex_auto_review::AutoReviewBudget;
use codex_auto_review::AutoReviewTerminalReason;
use codex_auto_review::AutoReviewUsage;
use codex_prompts::render_review_exit_interrupted;
use codex_prompts::render_review_exit_success;
use codex_protocol::ResponseItemId;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::items::ExitedReviewModeItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::BackgroundAutoReviewStatus;
use codex_protocol::protocol::BackgroundAutoReviewStatusEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::review_format::format_review_findings_block;
use codex_protocol::review_format::render_review_output_text;
use tokio_util::sync::CancellationToken;

use crate::codex_delegate::run_codex_thread_one_shot;
use crate::config::Constrained;
use crate::review_persistence::ReviewInterruptionStatus;
use crate::review_persistence::ReviewPersistenceContext;
use crate::review_persistence::SavedReviewInterruption;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use codex_features::Feature;
use codex_protocol::user_input::UserInput;

use super::SessionTask;
use super::SessionTaskContext;
use super::SessionTaskResult;
use super::background_review_budget::BackgroundReviewBudgetGate;

const BACKGROUND_AUTO_REVIEW_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct ReviewTask {
    persistence: Option<ReviewPersistenceContext>,
}

struct ReviewExecution {
    output: Option<ReviewOutputEvent>,
    token_usage: Option<TokenUsage>,
    usage: AutoReviewUsage,
    terminal_reason: Option<AutoReviewTerminalReason>,
}

struct ReviewConversation {
    session: Arc<Session>,
    receiver: async_channel::Receiver<Event>,
    budget_gate: Option<BackgroundReviewBudgetGate>,
}

impl ReviewTask {
    pub(crate) fn new() -> Self {
        Self { persistence: None }
    }

    pub(crate) fn with_persistence(persistence: ReviewPersistenceContext) -> Self {
        Self {
            persistence: Some(persistence),
        }
    }

    pub(crate) fn replace_persistence(mut self, persistence: ReviewPersistenceContext) -> Self {
        self.persistence = Some(persistence);
        self
    }

    pub(crate) fn persistence_context(&self) -> Option<ReviewPersistenceContext> {
        self.persistence.clone()
    }
}

impl SessionTask for ReviewTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Review
    }

    fn span_name(&self) -> &'static str {
        "session_task.review"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        run_review_task(
            self.persistence.clone(),
            session,
            ctx,
            input,
            cancellation_token,
        )
        .await
    }

    async fn abort(&self, session: Arc<SessionTaskContext>, ctx: Arc<TurnContext>) {
        let should_exit_review_mode = self
            .persistence
            .as_ref()
            .is_none_or(ReviewPersistenceContext::is_manual);
        if let Some(persistence) = self.persistence.clone() {
            let codex_home = session.codex_home().await;
            if let Some(interruption) = persistence.save_interrupted(codex_home) {
                send_interrupted_background_auto_review_status(
                    session.clone_session(),
                    &persistence,
                    interruption,
                )
                .await;
            }
        }
        if should_exit_review_mode {
            exit_review_mode(session.clone_session(), /*review_output*/ None, ctx).await;
        }
    }
}

async fn run_review_task(
    persistence: Option<ReviewPersistenceContext>,
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    input: Vec<TurnInput>,
    cancellation_token: CancellationToken,
) -> SessionTaskResult {
    session
        .session
        .services
        .session_telemetry
        .counter("codex.task.review", /*inc*/ 1, &[]);

    let mut user_input = Vec::new();
    for item in input {
        match item {
            TurnInput::UserInput { mut content, .. } => user_input.append(&mut content),
            TurnInput::ResponseItem(_) | TurnInput::InterAgentCommunication(_) => {}
        }
    }

    if let Some(persistence) = persistence.as_ref() {
        let codex_home = session.codex_home().await;
        if persistence.save_running(codex_home) {
            send_background_auto_review_status(
                session.clone_session(),
                persistence,
                BackgroundAutoReviewStatus::Running,
                /*error_summary*/ None,
            )
            .await;
        }
    }

    let review_started = tokio::time::Instant::now();
    let budget = persistence
        .as_ref()
        .and_then(ReviewPersistenceContext::background_budget)
        .cloned();
    let budget_gate = budget.clone().map(BackgroundReviewBudgetGate::new);
    let codex_home = session.codex_home().await;

    // Start sub-codex conversation and get the receiver for events.
    let start_conversation = Box::pin(start_review_conversation(
        session.clone(),
        ctx.clone(),
        user_input,
        cancellation_token.clone(),
        budget_gate.clone(),
    ));
    let start_result: Result<Option<ReviewConversation>, ReviewExecution> =
        if let Some(budget) = &budget {
            let remaining = Duration::from_millis(budget.max_elapsed_ms)
                .saturating_sub(review_started.elapsed());
            tokio::select! {
                biased;
                review_codex = start_conversation => Ok(review_codex),
                _ = tokio::time::sleep(remaining) => {
                    cancellation_token.cancel();
                    Err(ReviewExecution {
                        output: None,
                        token_usage: None,
                        usage: review_usage(
                            /*token_usage*/ None,
                            budget_gate.as_ref(),
                            elapsed_millis(review_started.elapsed()),
                        ),
                        terminal_reason: Some(AutoReviewTerminalReason::BudgetElapsed),
                    })
                }
            }
        } else {
            Ok(start_conversation.await)
        };
    let execution = match start_result {
        Err(execution) => execution,
        Ok(Some(review_conversation)) => {
            Box::pin(execute_review_conversation(
                session.clone(),
                ctx.clone(),
                &review_conversation,
                persistence.as_ref(),
                &codex_home,
                budget.as_ref(),
                review_started,
                cancellation_token.clone(),
            ))
            .await
        }
        Ok(None) => {
            let elapsed_ms = elapsed_millis(review_started.elapsed());
            let terminal_reason = budget.as_ref().and_then(|budget| {
                elapsed_reaches_budget(elapsed_ms, budget)
                    .then_some(AutoReviewTerminalReason::BudgetElapsed)
            });
            ReviewExecution {
                output: None,
                token_usage: None,
                usage: review_usage(/*token_usage*/ None, budget_gate.as_ref(), elapsed_ms),
                terminal_reason,
            }
        }
    };
    let cancelled = cancellation_token.is_cancelled();
    let should_exit_review_mode = persistence
        .as_ref()
        .is_none_or(ReviewPersistenceContext::is_manual);
    if should_exit_review_mode && (!cancelled || execution.output.is_some()) {
        exit_review_mode(
            session.clone_session(),
            execution.output.clone(),
            ctx.clone(),
        )
        .await;
    }
    if let Some(persistence) = persistence {
        if let Some(output) = execution.output.as_ref() {
            let freshness = if persistence.is_background() {
                persistence.completion_freshness(&codex_home).await
            } else {
                codex_auto_review::AutoReviewRunFreshness::Current
            };
            if persistence.save_completed_with_freshness(
                &codex_home,
                output,
                execution.token_usage.as_ref(),
                freshness,
                execution.usage,
            ) {
                send_background_auto_review_status(
                    session.clone_session(),
                    &persistence,
                    BackgroundAutoReviewStatus::Completed,
                    /*error_summary*/ None,
                )
                .await;
            }
        } else if let Some(reason) = execution.terminal_reason {
            let error_summary =
                background_review_budget_summary(reason, budget.as_ref(), &execution.usage);
            if persistence.save_budget_cancelled(
                &codex_home,
                reason,
                error_summary.clone(),
                execution.token_usage.as_ref(),
                execution.usage,
            ) {
                send_background_auto_review_status(
                    session.clone_session(),
                    &persistence,
                    BackgroundAutoReviewStatus::Cancelled,
                    Some(error_summary),
                )
                .await;
            }
        } else if cancelled {
            if let Some(interruption) = persistence.save_interrupted(codex_home) {
                send_interrupted_background_auto_review_status(
                    session.clone_session(),
                    &persistence,
                    interruption,
                )
                .await;
            }
        } else {
            let error_summary = "review ended without producing review output".to_string();
            if persistence.save_empty_output(
                &codex_home,
                error_summary.clone(),
                execution.token_usage.as_ref(),
                execution.usage,
            ) {
                send_background_auto_review_status(
                    session.clone_session(),
                    &persistence,
                    BackgroundAutoReviewStatus::Failed,
                    Some(error_summary),
                )
                .await;
            }
        }
    }
    Ok(None)
}

async fn send_interrupted_background_auto_review_status(
    session: Arc<Session>,
    persistence: &ReviewPersistenceContext,
    interruption: SavedReviewInterruption,
) {
    let status = match interruption.status {
        ReviewInterruptionStatus::Cancelled => BackgroundAutoReviewStatus::Cancelled,
        ReviewInterruptionStatus::Superseded => BackgroundAutoReviewStatus::Superseded,
    };
    send_background_auto_review_status(
        session,
        persistence,
        status,
        Some(interruption.error_summary),
    )
    .await;
}

async fn send_background_auto_review_status(
    session: Arc<Session>,
    persistence: &ReviewPersistenceContext,
    status: BackgroundAutoReviewStatus,
    error_summary: Option<String>,
) {
    if !persistence.is_background() {
        return;
    }
    let event = Event {
        id: persistence.run_id().to_string(),
        msg: EventMsg::BackgroundAutoReviewStatus(BackgroundAutoReviewStatusEvent {
            run_id: persistence.run_id().to_string(),
            status,
            review_target: persistence.review_target().clone(),
            error_summary,
        }),
    };
    if let Err(err) = tokio::spawn(async move {
        session.send_event_raw(event).await;
    })
    .await
    {
        tracing::warn!(error = %err, "background auto review status task failed");
    }
}

async fn start_review_conversation(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    input: Vec<UserInput>,
    cancellation_token: CancellationToken,
    budget_gate: Option<BackgroundReviewBudgetGate>,
) -> Option<ReviewConversation> {
    let config = ctx.config.clone();
    let mut sub_agent_config = config.as_ref().clone();
    // Carry over review-only feature restrictions so the delegate cannot
    // re-enable blocked tools (web search, collab tools, view image).
    if let Err(err) = sub_agent_config
        .web_search_mode
        .set(WebSearchMode::Disabled)
    {
        panic!("by construction Constrained<WebSearchMode> must always support Disabled: {err}");
    }
    let _ = sub_agent_config.features.disable(Feature::SpawnCsv);
    let _ = sub_agent_config.features.disable(Feature::Collab);
    sub_agent_config.agents_enabled = false;
    if let Some(budget_gate) = budget_gate.as_ref() {
        let _ = sub_agent_config.features.disable(Feature::CodeModeOnly);
        let _ = sub_agent_config.features.disable(Feature::CodeMode);
        let _ = sub_agent_config.features.disable(Feature::Apps);
        let _ = sub_agent_config.features.disable(Feature::EnableMcpApps);
        sub_agent_config.include_skill_instructions = false;
        sub_agent_config.orchestrator_skills_enabled = false;
        sub_agent_config.include_apps_instructions = false;
        sub_agent_config.mcp_servers = Constrained::allow_only(Default::default());
        sub_agent_config.tool_output_token_limit = Some(
            sub_agent_config
                .tool_output_token_limit
                .unwrap_or(usize::MAX)
                .min(budget_gate.tool_output_limit_tokens()),
        );
    }

    // Set explicit review rubric for the sub-agent
    sub_agent_config.base_instructions = Some(crate::REVIEW_PROMPT.to_string());
    sub_agent_config.permissions.approval_policy = Constrained::allow_only(AskForApproval::Never);

    let model = config
        .review_model
        .clone()
        .unwrap_or_else(|| ctx.model_info.slug.clone());
    sub_agent_config.model = Some(model);
    let mut thread_extension_init = codex_extension_api::ExtensionDataInit::new();
    if let Some(budget_gate) = budget_gate.clone() {
        thread_extension_init.insert(budget_gate);
    }
    (run_codex_thread_one_shot(
        sub_agent_config,
        session.auth_manager(),
        input,
        session.clone_session(),
        ctx.clone(),
        cancellation_token,
        SubAgentSource::Review,
        /*final_output_json_schema*/ None,
        /*initial_history*/ None,
        thread_extension_init,
    )
    .await)
        .ok()
        .map(|(session, io)| ReviewConversation {
            session,
            receiver: io.rx_event,
            budget_gate,
        })
}

async fn process_review_events(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    review_conversation: &ReviewConversation,
) -> Option<String> {
    let mut prev_agent_message: Option<Event> = None;
    while let Ok(event) = review_conversation.receiver.recv().await {
        match event.clone().msg {
            EventMsg::AgentMessage(_) => {
                if let Some(prev) = prev_agent_message.take() {
                    session
                        .clone_session()
                        .send_event(ctx.as_ref(), prev.msg)
                        .await;
                }
                prev_agent_message = Some(event);
            }
            // Suppress child turn starts and assistant message events that have
            // dedicated handling in the parent review flow.
            EventMsg::TurnStarted(_)
            | EventMsg::ItemCompleted(ItemCompletedEvent {
                item: TurnItem::AgentMessage(_),
                ..
            })
            | EventMsg::AgentMessageContentDelta(AgentMessageContentDeltaEvent { .. }) => {}
            EventMsg::TurnComplete(task_complete) => {
                return task_complete.last_agent_message;
            }
            EventMsg::TurnAborted(_) => {
                // Cancellation or abort: consumer will finalize with None.
                return None;
            }
            other => {
                session
                    .clone_session()
                    .send_event(ctx.as_ref(), other)
                    .await;
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn execute_review_conversation(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    review_conversation: &ReviewConversation,
    persistence: Option<&ReviewPersistenceContext>,
    codex_home: &std::path::Path,
    budget: Option<&AutoReviewBudget>,
    started: tokio::time::Instant,
    cancellation_token: CancellationToken,
) -> ReviewExecution {
    let events = Box::pin(process_review_events(session, ctx, review_conversation));
    let raw_output = if let Some(budget) = budget {
        let monitor = Box::pin(wait_for_review_budget(
            review_conversation,
            persistence,
            codex_home,
            budget,
            started,
        ));
        tokio::select! {
            biased;
            raw_output = events => raw_output,
            execution = monitor => {
                cancellation_token.cancel();
                return execution;
            }
        }
    } else {
        events.await
    };
    let token_usage = review_conversation
        .session
        .token_usage_info()
        .await
        .map(|info| info.total_token_usage);
    let total_tokens = token_usage.as_ref().and_then(review_token_count);
    let elapsed_ms = elapsed_millis(started.elapsed());
    if budget.is_some_and(|budget| elapsed_exceeds_budget(elapsed_ms, budget)) {
        return ReviewExecution {
            output: None,
            token_usage: token_usage.clone(),
            usage: review_usage(
                token_usage.as_ref(),
                review_conversation.budget_gate.as_ref(),
                elapsed_ms,
            ),
            terminal_reason: Some(AutoReviewTerminalReason::BudgetElapsed),
        };
    }
    if budget.is_some_and(|budget| {
        total_tokens.is_some_and(|total_tokens| total_tokens_exceed_budget(total_tokens, budget))
    }) {
        return ReviewExecution {
            output: None,
            token_usage: token_usage.clone(),
            usage: review_usage(
                token_usage.as_ref(),
                review_conversation.budget_gate.as_ref(),
                elapsed_ms,
            ),
            terminal_reason: Some(AutoReviewTerminalReason::BudgetTotalTokens),
        };
    }
    if raw_output.is_none()
        && review_conversation
            .budget_gate
            .as_ref()
            .is_some_and(BackgroundReviewBudgetGate::stopped)
    {
        return ReviewExecution {
            output: None,
            token_usage: token_usage.clone(),
            usage: review_usage(
                token_usage.as_ref(),
                review_conversation.budget_gate.as_ref(),
                elapsed_ms,
            ),
            terminal_reason: Some(AutoReviewTerminalReason::BudgetTotalTokens),
        };
    }
    let Some(raw_output) = raw_output else {
        return ReviewExecution {
            output: None,
            token_usage: token_usage.clone(),
            usage: review_usage(
                token_usage.as_ref(),
                review_conversation.budget_gate.as_ref(),
                elapsed_ms,
            ),
            terminal_reason: None,
        };
    };
    let output_bytes = raw_output.len();
    if budget.is_some_and(|budget| raw_output_exceeds_budget(&raw_output, budget)) {
        return ReviewExecution {
            output: None,
            token_usage: token_usage.clone(),
            usage: review_usage_with_output(
                token_usage.as_ref(),
                review_conversation.budget_gate.as_ref(),
                elapsed_ms,
                Some(output_bytes),
                /*finding_count*/ None,
            ),
            terminal_reason: Some(AutoReviewTerminalReason::BudgetOutput),
        };
    }
    let output = parse_review_output_event(&raw_output);
    let finding_count = output.findings.len();
    if budget.is_some_and(|budget| findings_exceed_budget(&output, budget)) {
        return ReviewExecution {
            output: None,
            token_usage: token_usage.clone(),
            usage: review_usage_with_output(
                token_usage.as_ref(),
                review_conversation.budget_gate.as_ref(),
                elapsed_ms,
                Some(output_bytes),
                Some(finding_count),
            ),
            terminal_reason: Some(AutoReviewTerminalReason::BudgetFindingCount),
        };
    }
    ReviewExecution {
        output: Some(output),
        token_usage: token_usage.clone(),
        usage: review_usage_with_output(
            token_usage.as_ref(),
            review_conversation.budget_gate.as_ref(),
            elapsed_ms,
            Some(output_bytes),
            Some(finding_count),
        ),
        terminal_reason: None,
    }
}

async fn wait_for_review_budget(
    review_conversation: &ReviewConversation,
    persistence: Option<&ReviewPersistenceContext>,
    codex_home: &std::path::Path,
    budget: &AutoReviewBudget,
    started: tokio::time::Instant,
) -> ReviewExecution {
    let mut last_persisted_elapsed_ms = 0;
    let mut last_persisted_total_tokens = None;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let elapsed_ms = elapsed_millis(started.elapsed());
        let token_usage = review_conversation
            .session
            .token_usage_info()
            .await
            .map(|info| info.total_token_usage);
        let total_tokens = token_usage.as_ref().and_then(review_token_count);
        let usage = review_usage(
            token_usage.as_ref(),
            review_conversation.budget_gate.as_ref(),
            elapsed_ms,
        );
        if let Some(persistence) = persistence
            && (elapsed_ms.saturating_sub(last_persisted_elapsed_ms)
                >= elapsed_millis(BACKGROUND_AUTO_REVIEW_PROGRESS_INTERVAL)
                || total_tokens != last_persisted_total_tokens)
        {
            persistence.record_progress(codex_home, usage.clone());
            last_persisted_elapsed_ms = elapsed_ms;
            last_persisted_total_tokens = total_tokens;
        }
        if elapsed_reaches_budget(elapsed_ms, budget) {
            return ReviewExecution {
                output: None,
                token_usage,
                usage,
                terminal_reason: Some(AutoReviewTerminalReason::BudgetElapsed),
            };
        }
        if review_conversation
            .budget_gate
            .as_ref()
            .is_some_and(|gate| gate.observe_usage(token_usage.as_ref()))
        {
            return ReviewExecution {
                output: None,
                token_usage,
                usage,
                terminal_reason: Some(AutoReviewTerminalReason::BudgetTotalTokens),
            };
        }
        if total_tokens.is_some_and(|total_tokens| total_tokens_reach_budget(total_tokens, budget))
        {
            return ReviewExecution {
                output: None,
                token_usage,
                usage,
                terminal_reason: Some(AutoReviewTerminalReason::BudgetTotalTokens),
            };
        }
    }
}

fn review_usage(
    token_usage: Option<&TokenUsage>,
    budget_gate: Option<&BackgroundReviewBudgetGate>,
    elapsed_ms: u64,
) -> AutoReviewUsage {
    review_usage_with_output(
        token_usage,
        budget_gate,
        elapsed_ms,
        /*output_bytes*/ None,
        /*finding_count*/ None,
    )
}

fn review_usage_with_output(
    token_usage: Option<&TokenUsage>,
    budget_gate: Option<&BackgroundReviewBudgetGate>,
    elapsed_ms: u64,
    output_bytes: Option<usize>,
    finding_count: Option<usize>,
) -> AutoReviewUsage {
    let mut usage = budget_gate
        .map(|gate| gate.usage(token_usage))
        .unwrap_or_default();
    usage.elapsed_ms = Some(elapsed_ms);
    usage.total_tokens = token_usage.and_then(review_token_count);
    usage.input_tokens = token_usage.and_then(|usage| positive_tokens(usage.input_tokens));
    usage.cached_input_tokens =
        token_usage.and_then(|usage| positive_tokens(usage.cached_input_tokens));
    usage.output_tokens = token_usage.and_then(|usage| positive_tokens(usage.output_tokens));
    usage.output_bytes = output_bytes;
    usage.finding_count = finding_count;
    usage
}

fn positive_tokens(tokens: i64) -> Option<u64> {
    u64::try_from(tokens).ok().filter(|tokens| *tokens > 0)
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

fn elapsed_reaches_budget(elapsed_ms: u64, budget: &AutoReviewBudget) -> bool {
    elapsed_ms >= budget.max_elapsed_ms
}

fn elapsed_exceeds_budget(elapsed_ms: u64, budget: &AutoReviewBudget) -> bool {
    elapsed_ms > budget.max_elapsed_ms
}

fn total_tokens_reach_budget(total_tokens: u64, budget: &AutoReviewBudget) -> bool {
    total_tokens >= budget.max_total_tokens
}

fn total_tokens_exceed_budget(total_tokens: u64, budget: &AutoReviewBudget) -> bool {
    total_tokens > budget.max_total_tokens
}

fn raw_output_exceeds_budget(raw_output: &str, budget: &AutoReviewBudget) -> bool {
    raw_output.len() > budget.max_output_bytes
}

fn findings_exceed_budget(output: &ReviewOutputEvent, budget: &AutoReviewBudget) -> bool {
    output.findings.len() > budget.max_findings
}

fn elapsed_millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn background_review_budget_summary(
    reason: AutoReviewTerminalReason,
    budget: Option<&AutoReviewBudget>,
    usage: &AutoReviewUsage,
) -> String {
    let Some(budget) = budget else {
        return "background review stopped after exceeding its execution budget".to_string();
    };
    match reason {
        AutoReviewTerminalReason::BudgetElapsed => format!(
            "background review exceeded elapsed budget: {} ms >= {} ms; retry after narrowing the review scope or increasing `auto_review.background_max_elapsed_seconds`",
            usage.elapsed_ms.unwrap_or_default(),
            budget.max_elapsed_ms
        ),
        AutoReviewTerminalReason::BudgetTotalTokens => {
            let effective_limit = usage
                .effective_total_token_limit
                .unwrap_or(budget.max_total_tokens);
            let projected = usage
                .projected_total_tokens
                .unwrap_or_else(|| usage.total_tokens.unwrap_or_default());
            format!(
                "background review stopped at its effective token budget: consumed {} tokens, projected {projected}, effective ceiling {effective_limit}, hard ceiling {} ({} token accounting tolerance); retry with a narrower diff or increase `auto_review.background_max_total_tokens`",
                usage.total_tokens.unwrap_or_default(),
                budget.max_total_tokens,
                usage.accounting_tolerance_tokens.unwrap_or_default()
            )
        }
        AutoReviewTerminalReason::BudgetOutput => format!(
            "background review exceeded output budget: {} bytes > {} bytes; retry with a narrower diff or increase `auto_review.background_max_output_bytes`",
            usage.output_bytes.unwrap_or_default(),
            budget.max_output_bytes
        ),
        AutoReviewTerminalReason::BudgetFindingCount => format!(
            "background review exceeded finding budget: {} findings > {} findings; retry with a narrower diff or increase `auto_review.background_max_findings`",
            usage.finding_count.unwrap_or_default(),
            budget.max_findings
        ),
        AutoReviewTerminalReason::BudgetScope
        | AutoReviewTerminalReason::EmptyOutput
        | AutoReviewTerminalReason::StaleTarget => {
            "background review stopped after exceeding its execution budget".to_string()
        }
    }
}

/// Parse a ReviewOutputEvent from a text blob returned by the reviewer model.
/// If the text is valid JSON matching ReviewOutputEvent, deserialize it.
/// Otherwise, attempt to extract the first JSON object substring and parse it.
/// If parsing still fails, return a structured fallback carrying the plain text
/// in `overall_explanation`.
fn parse_review_output_event(text: &str) -> ReviewOutputEvent {
    if let Ok(ev) = serde_json::from_str::<ReviewOutputEvent>(text) {
        return ev;
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}'))
        && start < end
        && let Some(slice) = text.get(start..=end)
        && let Ok(ev) = serde_json::from_str::<ReviewOutputEvent>(slice)
    {
        return ev;
    }
    ReviewOutputEvent {
        overall_explanation: text.to_string(),
        ..Default::default()
    }
}

/// Emits ExitedReviewMode item lifecycle with optional ReviewOutput,
/// and records the review output back into conversation history.
pub(crate) async fn exit_review_mode(
    session: Arc<Session>,
    review_output: Option<ReviewOutputEvent>,
    ctx: Arc<TurnContext>,
) {
    let (user_message, assistant_message) = if let Some(out) = review_output.clone() {
        let mut findings_str = String::new();
        let text = out.overall_explanation.trim();
        if !text.is_empty() {
            findings_str.push_str(text);
        }
        if !out.findings.is_empty() {
            let block = format_review_findings_block(&out.findings, /*selection*/ None);
            findings_str.push_str(&format!("\n{block}"));
        }
        let rendered = render_review_exit_success(&findings_str);
        let assistant_message = render_review_output_text(&out);
        (rendered, assistant_message)
    } else {
        let rendered = render_review_exit_interrupted();
        let assistant_message =
            "Review was interrupted. Please re-run /review and wait for it to complete."
                .to_string();
        (rendered, assistant_message)
    };

    session
        .record_conversation_items(
            &ctx,
            &[ResponseItem::Message {
                id: Some(ResponseItemId::new("msg")),
                role: "user".to_string(),
                content: vec![ContentItem::InputText { text: user_message }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            }],
        )
        .await;

    let item = TurnItem::ExitedReviewMode(ExitedReviewModeItem {
        id: uuid::Uuid::now_v7().to_string(),
        review_output,
    });
    session.emit_turn_item_started(ctx.as_ref(), &item).await;
    session.emit_turn_item_completed(ctx.as_ref(), item).await;
    session
        .record_response_item_and_emit_turn_item(
            ctx.as_ref(),
            ResponseItem::Message {
                id: Some(ResponseItemId::new("msg")),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: assistant_message,
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            },
        )
        .await;

    // Review turns can run before any regular user turn, so explicitly
    // materialize rollout persistence. Do this after emitting review output so
    // file creation + git metadata collection cannot delay client-facing items.
    session.ensure_rollout_materialized().await;
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod review_tests;
