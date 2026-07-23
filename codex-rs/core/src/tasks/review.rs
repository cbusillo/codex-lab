use std::sync::Arc;
use std::time::Duration;

use codex_auto_review::AutoReviewBudget;
use codex_auto_review::AutoReviewTerminalReason;
use codex_auto_review::AutoReviewUsage;
use codex_prompts::render_review_exit_interrupted;
use codex_prompts::render_review_exit_success;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::BackgroundAutoReviewStatus;
use codex_protocol::protocol::BackgroundAutoReviewStatusEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExitedReviewModeEvent;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::TokenUsage;
use tokio_util::sync::CancellationToken;

use crate::codex_delegate::run_codex_thread_one_shot;
use crate::config::Constrained;
use crate::review_format::format_review_findings_block;
use crate::review_format::render_review_output_text;
use crate::review_persistence::ReviewInterruptionStatus;
use crate::review_persistence::ReviewPersistenceContext;
use crate::review_persistence::SavedReviewInterruption;
use crate::session::Codex;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use codex_features::Feature;
use codex_protocol::user_input::UserInput;

use super::SessionTask;
use super::SessionTaskContext;

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
    ) -> Option<String> {
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
) -> Option<String> {
    session
        .session
        .services
        .session_telemetry
        .counter("codex.task.review", /*inc*/ 1, &[]);

    let mut user_input = Vec::new();
    for item in input {
        match item {
            TurnInput::UserInput { mut content, .. } => user_input.append(&mut content),
            TurnInput::ResponseItem(_) => {}
        }
    }

    if let Some(persistence) = persistence.as_ref() {
        let codex_home = session.codex_home().await;
        if persistence.save_running(codex_home) {
            send_background_auto_review_status(
                session.clone_session(),
                persistence,
                BackgroundAutoReviewStatus::Running,
                None,
            )
            .await;
        }
    }

    let review_started = tokio::time::Instant::now();
    let budget = persistence
        .as_ref()
        .and_then(ReviewPersistenceContext::background_budget)
        .cloned();
    let codex_home = session.codex_home().await;

    // Start sub-codex conversation and get the receiver for events.
    let start_conversation = Box::pin(start_review_conversation(
        session.clone(),
        ctx.clone(),
        user_input,
        cancellation_token.clone(),
    ));
    let start_result: Result<Option<Codex>, ReviewExecution> = if let Some(budget) = &budget {
        let remaining =
            Duration::from_millis(budget.max_elapsed_ms).saturating_sub(review_started.elapsed());
        tokio::select! {
            biased;
            review_codex = start_conversation => Ok(review_codex),
            _ = tokio::time::sleep(remaining) => {
                cancellation_token.cancel();
                Err(ReviewExecution {
                    output: None,
                    token_usage: None,
                    usage: AutoReviewUsage {
                        elapsed_ms: Some(elapsed_millis(review_started.elapsed())),
                        ..Default::default()
                    },
                    terminal_reason: Some(AutoReviewTerminalReason::BudgetElapsed),
                })
            }
        }
    } else {
        Ok(start_conversation.await)
    };
    let execution = match start_result {
        Err(execution) => execution,
        Ok(Some(review_codex)) => {
            Box::pin(execute_review_conversation(
                session.clone(),
                ctx.clone(),
                &review_codex,
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
                usage: AutoReviewUsage {
                    elapsed_ms: Some(elapsed_ms),
                    ..Default::default()
                },
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
                    None,
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
    None
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
) -> Option<Codex> {
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
    let _ = sub_agent_config.features.disable(Feature::MultiAgentV2);

    // Set explicit review rubric for the sub-agent
    sub_agent_config.base_instructions = Some(crate::REVIEW_PROMPT.to_string());
    sub_agent_config.permissions.approval_policy = Constrained::allow_only(AskForApproval::Never);

    let model = config
        .review_model
        .clone()
        .unwrap_or_else(|| ctx.model_info.slug.clone());
    sub_agent_config.model = Some(model);
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
    )
    .await)
        .ok()
}

async fn process_review_events(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    review_codex: &Codex,
) -> Option<String> {
    let mut prev_agent_message: Option<Event> = None;
    while let Ok(event) = review_codex.next_event().await {
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

async fn execute_review_conversation(
    session: Arc<SessionTaskContext>,
    ctx: Arc<TurnContext>,
    review_codex: &Codex,
    persistence: Option<&ReviewPersistenceContext>,
    codex_home: &std::path::Path,
    budget: Option<&AutoReviewBudget>,
    started: tokio::time::Instant,
    cancellation_token: CancellationToken,
) -> ReviewExecution {
    let events = Box::pin(process_review_events(session, ctx, review_codex));
    let raw_output = if let Some(budget) = budget {
        let monitor = Box::pin(wait_for_review_budget(
            review_codex,
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
    let token_usage = review_codex
        .session
        .token_usage_info()
        .await
        .map(|info| info.total_token_usage);
    let total_tokens = token_usage.as_ref().and_then(review_token_count);
    let elapsed_ms = elapsed_millis(started.elapsed());
    if budget.is_some_and(|budget| elapsed_exceeds_budget(elapsed_ms, budget)) {
        return ReviewExecution {
            output: None,
            token_usage,
            usage: AutoReviewUsage {
                elapsed_ms: Some(elapsed_ms),
                total_tokens,
                ..Default::default()
            },
            terminal_reason: Some(AutoReviewTerminalReason::BudgetElapsed),
        };
    }
    if budget.is_some_and(|budget| {
        total_tokens.is_some_and(|total_tokens| total_tokens_exceed_budget(total_tokens, budget))
    }) {
        return ReviewExecution {
            output: None,
            token_usage,
            usage: AutoReviewUsage {
                elapsed_ms: Some(elapsed_ms),
                total_tokens,
                ..Default::default()
            },
            terminal_reason: Some(AutoReviewTerminalReason::BudgetTotalTokens),
        };
    }
    let Some(raw_output) = raw_output else {
        return ReviewExecution {
            output: None,
            token_usage,
            usage: AutoReviewUsage {
                elapsed_ms: Some(elapsed_ms),
                total_tokens,
                ..Default::default()
            },
            terminal_reason: None,
        };
    };
    let output_bytes = raw_output.len();
    if budget.is_some_and(|budget| raw_output_exceeds_budget(&raw_output, budget)) {
        return ReviewExecution {
            output: None,
            token_usage,
            usage: AutoReviewUsage {
                elapsed_ms: Some(elapsed_ms),
                total_tokens,
                output_bytes: Some(output_bytes),
                ..Default::default()
            },
            terminal_reason: Some(AutoReviewTerminalReason::BudgetOutput),
        };
    }
    let output = parse_review_output_event(&raw_output);
    let finding_count = output.findings.len();
    if budget.is_some_and(|budget| findings_exceed_budget(&output, budget)) {
        return ReviewExecution {
            output: None,
            token_usage,
            usage: AutoReviewUsage {
                elapsed_ms: Some(elapsed_ms),
                total_tokens,
                output_bytes: Some(output_bytes),
                finding_count: Some(finding_count),
                ..Default::default()
            },
            terminal_reason: Some(AutoReviewTerminalReason::BudgetFindingCount),
        };
    }
    ReviewExecution {
        output: Some(output),
        token_usage,
        usage: AutoReviewUsage {
            elapsed_ms: Some(elapsed_ms),
            total_tokens,
            output_bytes: Some(output_bytes),
            finding_count: Some(finding_count),
            ..Default::default()
        },
        terminal_reason: None,
    }
}

async fn wait_for_review_budget(
    review_codex: &Codex,
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
        let token_usage = review_codex
            .session
            .token_usage_info()
            .await
            .map(|info| info.total_token_usage);
        let total_tokens = token_usage.as_ref().and_then(review_token_count);
        let usage = AutoReviewUsage {
            elapsed_ms: Some(elapsed_ms),
            total_tokens,
            ..Default::default()
        };
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
            "background review exceeded elapsed budget: {} ms >= {} ms",
            usage.elapsed_ms.unwrap_or_default(),
            budget.max_elapsed_ms
        ),
        AutoReviewTerminalReason::BudgetTotalTokens => format!(
            "background review exceeded token budget: {} tokens >= {} tokens",
            usage.total_tokens.unwrap_or_default(),
            budget.max_total_tokens
        ),
        AutoReviewTerminalReason::BudgetOutput => format!(
            "background review exceeded output budget: {} bytes > {} bytes",
            usage.output_bytes.unwrap_or_default(),
            budget.max_output_bytes
        ),
        AutoReviewTerminalReason::BudgetFindingCount => format!(
            "background review exceeded finding budget: {} findings > {} findings",
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

#[cfg(test)]
mod budget_tests {
    use super::*;
    use codex_protocol::protocol::ReviewCodeLocation;
    use codex_protocol::protocol::ReviewFinding;
    use codex_protocol::protocol::ReviewLineRange;
    use std::path::PathBuf;

    #[test]
    fn oversized_output_exceeds_budget() {
        let budget = test_budget();
        assert!(raw_output_exceeds_budget(
            &"x".repeat(budget.max_output_bytes + 1),
            &budget
        ));
        assert!(!raw_output_exceeds_budget(
            &"x".repeat(budget.max_output_bytes),
            &budget
        ));
    }

    #[test]
    fn excess_findings_exceed_budget() {
        let budget = AutoReviewBudget {
            max_findings: 1,
            ..test_budget()
        };
        let finding = ReviewFinding {
            title: "[P1] Finding".to_string(),
            body: "body".to_string(),
            confidence_score: 0.9,
            priority: 1,
            code_location: ReviewCodeLocation {
                absolute_file_path: PathBuf::from("/tmp/src/lib.rs"),
                line_range: ReviewLineRange { start: 1, end: 1 },
            },
        };
        let output = ReviewOutputEvent {
            findings: vec![finding.clone(), finding],
            ..Default::default()
        };

        assert!(findings_exceed_budget(&output, &budget));
    }

    #[test]
    fn budget_summary_is_structured_and_bounded() {
        let summary = background_review_budget_summary(
            AutoReviewTerminalReason::BudgetTotalTokens,
            Some(&test_budget()),
            &AutoReviewUsage {
                total_tokens: Some(250_001),
                ..Default::default()
            },
        );
        assert_eq!(
            summary,
            "background review exceeded token budget: 250001 tokens >= 250000 tokens"
        );
    }

    #[test]
    fn elapsed_budget_is_enforced_at_the_deadline() {
        let budget = test_budget();
        assert!(!elapsed_reaches_budget(budget.max_elapsed_ms - 1, &budget));
        assert!(elapsed_reaches_budget(budget.max_elapsed_ms, &budget));
        assert!(!elapsed_exceeds_budget(budget.max_elapsed_ms, &budget));
        assert!(elapsed_exceeds_budget(budget.max_elapsed_ms + 1, &budget));
    }

    #[test]
    fn token_count_includes_cached_input() {
        let token_usage = TokenUsage {
            input_tokens: 100,
            cached_input_tokens: 90,
            output_tokens: 10,
            reasoning_output_tokens: 0,
            total_tokens: 20,
        };

        assert_eq!(review_token_count(&token_usage), Some(110));
    }

    #[test]
    fn completed_output_at_exact_token_limit_is_allowed() {
        let budget = test_budget();
        assert!(total_tokens_reach_budget(budget.max_total_tokens, &budget));
        assert!(!total_tokens_exceed_budget(
            budget.max_total_tokens,
            &budget
        ));
        assert!(total_tokens_exceed_budget(
            budget.max_total_tokens + 1,
            &budget
        ));
    }

    fn test_budget() -> AutoReviewBudget {
        AutoReviewBudget {
            max_scope_bytes: 120_000,
            max_elapsed_ms: 300_000,
            max_total_tokens: 250_000,
            max_output_bytes: 64,
            max_findings: 20,
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

/// Emits an ExitedReviewMode Event with optional ReviewOutput,
/// and records a developer message with the review output.
pub(crate) async fn exit_review_mode(
    session: Arc<Session>,
    review_output: Option<ReviewOutputEvent>,
    ctx: Arc<TurnContext>,
) {
    const REVIEW_USER_MESSAGE_ID: &str = "review_rollout_user";
    const REVIEW_ASSISTANT_MESSAGE_ID: &str = "review_rollout_assistant";
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
                id: Some(REVIEW_USER_MESSAGE_ID.to_string()),
                role: "user".to_string(),
                content: vec![ContentItem::InputText { text: user_message }],
                phase: None,
            }],
        )
        .await;

    session
        .send_event(
            ctx.as_ref(),
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent { review_output }),
        )
        .await;
    session
        .record_response_item_and_emit_turn_item(
            ctx.as_ref(),
            ResponseItem::Message {
                id: Some(REVIEW_ASSISTANT_MESSAGE_ID.to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: assistant_message,
                }],
                phase: None,
            },
        )
        .await;

    // Review turns can run before any regular user turn, so explicitly
    // materialize rollout persistence. Do this after emitting review output so
    // file creation + git metadata collection cannot delay client-facing items.
    session.ensure_rollout_materialized().await;
}
