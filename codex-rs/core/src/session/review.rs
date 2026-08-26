use super::*;
use std::sync::atomic::AtomicBool;

use codex_auto_review::ReviewCoordination;
use codex_auto_review::ReviewLockGuard;
use codex_protocol::protocol::BackgroundAutoReviewStatus;
use codex_protocol::protocol::BackgroundAutoReviewStatusEvent;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::ReviewPersistence;
use codex_protocol::protocol::ReviewRequest;

use crate::review_persistence::ReviewPersistenceContext;
use crate::state::BackgroundAutoReviewRunningHandle;
use crate::tasks::SessionTask;

const ESTIMATED_BYTES_PER_REVIEW_PROMPT_TOKEN: u64 = 4;

pub(super) struct PreparedReviewThread {
    pub(super) turn_context: Arc<TurnContext>,
    pub(super) input: Vec<TurnInput>,
    pub(super) task: ReviewTask,
    manual_review_request: Option<ReviewRequest>,
}

impl PreparedReviewThread {
    pub(super) fn with_persistence(mut self, persistence: ReviewPersistenceContext) -> Self {
        self.task = self.task.replace_persistence(persistence);
        self
    }
}

pub(super) enum ReviewPersistenceSpec {
    Mode(ReviewPersistence),
    Context(Box<ReviewPersistenceContext>),
}

/// Spawn a review thread using the given prompt.
pub(super) async fn spawn_review_thread(
    sess: Arc<Session>,
    config: Arc<Config>,
    parent_turn_context: Arc<TurnContext>,
    sub_id: String,
    resolved: crate::review_prompts::ResolvedReviewRequest,
    persistence: Option<ReviewPersistence>,
) {
    let mut prepared = prepare_review_thread(
        Arc::clone(&sess),
        config,
        parent_turn_context,
        sub_id,
        resolved,
        persistence.map(ReviewPersistenceSpec::Mode),
    )
    .await;

    if let Some(review_request) = prepared.manual_review_request.clone() {
        sess.cancel_background_auto_review_for_foreground_work()
            .await;
        if let Some(persistence) = prepared.task.persistence_context()
            && persistence.is_manual()
        {
            prepared = match record_started_manual_auto_review(&sess, persistence).await {
                Some(persistence) => prepared.with_persistence(persistence),
                None => {
                    sess.send_event(
                        prepared.turn_context.as_ref(),
                        EventMsg::Error(ErrorEvent {
                            message: "failed to start persisted manual review".to_string(),
                            codex_error_info: Some(CodexErrorInfo::Other),
                        }),
                    )
                    .await;
                    return;
                }
            };
        }
        sess.abort_all_tasks(TurnAbortReason::Replaced).await;
        sess.clear_connector_selection().await;
        let turn_context = Arc::clone(&prepared.turn_context);
        sess.start_task(
            prepared.turn_context,
            prepared.input,
            prepared.task,
            crate::tasks::MailboxParentProvenance::Ignore,
        )
        .await;
        let item = TurnItem::EnteredReviewMode(EnteredReviewModeItem {
            id: uuid::Uuid::now_v7().to_string(),
            target: review_request.target,
            user_facing_hint: review_request.user_facing_hint.unwrap_or_default(),
        });
        sess.emit_turn_item_started(turn_context.as_ref(), &item)
            .await;
        sess.emit_turn_item_completed(turn_context.as_ref(), item)
            .await;
    } else {
        sess.spawn_task(prepared.turn_context, prepared.input, prepared.task)
            .await;
    }
}

pub(super) async fn prepare_review_thread(
    sess: Arc<Session>,
    config: Arc<Config>,
    parent_turn_context: Arc<TurnContext>,
    sub_id: String,
    resolved: crate::review_prompts::ResolvedReviewRequest,
    persistence: Option<ReviewPersistenceSpec>,
) -> PreparedReviewThread {
    let model = config
        .review_model
        .clone()
        .unwrap_or_else(|| parent_turn_context.model_info.slug.clone());
    let available_models = sess
        .services
        .models_manager
        .list_models(
            RefreshStrategy::OnlineIfUncached,
            config.http_client_factory(),
        )
        .await;
    let review_model_info = sess
        .services
        .models_manager
        .get_model_info(&model, &config.to_models_manager_config())
        .await;
    // For reviews, disable web_search and view_image regardless of global settings.
    let mut review_features = sess.features.clone();
    let _ = review_features.disable(Feature::WebSearchRequest);
    let _ = review_features.disable(Feature::WebSearchCached);
    let _ = review_features.disable(Feature::Goals);
    let review_web_search_mode = WebSearchMode::Disabled;
    let unified_exec_shell_mode = UnifiedExecShellMode::for_session(
        review_features.get(),
        crate::tools::tool_user_shell_type(sess.services.user_shell.as_ref()),
        sess.services.shell_zsh_path.as_ref(),
        sess.services.main_execve_wrapper_exe.as_ref(),
    );

    let review_prompt = resolved.prompt.clone();
    let provider = parent_turn_context.provider.clone();
    let auth_manager = parent_turn_context.auth_manager.clone();
    let model_info = review_model_info.clone();

    // Build per‑turn client with the requested model/family.
    let mut per_turn_config = (*parent_turn_context.config).clone();
    // Preserve configured overrides without carrying over the parent model's defaults.
    per_turn_config.token_budget = config.token_budget.clone();
    per_turn_config.model = Some(model.clone());
    per_turn_config.features = review_features.clone();
    if let Some(current_effort) = per_turn_config.model_reasoning_effort.as_ref()
        && review_model_info.slug != parent_turn_context.model_info.slug
        && !review_model_info.used_fallback_model_metadata
        && !review_model_info
            .supported_reasoning_levels
            .iter()
            .any(|preset| &preset.effort == current_effort)
    {
        let supported_reasoning_levels = &review_model_info.supported_reasoning_levels;
        per_turn_config.model_reasoning_effort = supported_reasoning_levels
            .get(supported_reasoning_levels.len().saturating_sub(1) / 2)
            .map(|preset| preset.effort.clone())
            .or_else(|| review_model_info.default_reasoning_level.clone());
    }
    if let Err(err) = per_turn_config.web_search_mode.set(review_web_search_mode) {
        let fallback_value = per_turn_config.web_search_mode.value();
        tracing::warn!(
            error = %err,
            ?review_web_search_mode,
            ?fallback_value,
            "review web_search_mode is disallowed by requirements; keeping constrained value"
        );
    }

    let session_telemetry = parent_turn_context
        .session_telemetry
        .clone()
        .with_model(model.as_str(), review_model_info.slug.as_str());
    let auth_manager_for_context = auth_manager.clone();
    let provider_for_context = provider.clone();
    let session_telemetry_for_context = session_telemetry.clone();
    let reasoning_effort = per_turn_config.model_reasoning_effort.clone();
    let reasoning_summary = per_turn_config
        .model_reasoning_summary
        .unwrap_or(model_info.default_reasoning_summary);
    let session_source = parent_turn_context.session_source.clone();
    let (forked_from_thread_id, thread_source, service_tier) = {
        let state = sess.state.lock().await;
        (
            state.session_configuration.forked_from_thread_id,
            state.session_configuration.thread_source.clone(),
            state
                .session_configuration
                .service_tier
                .clone()
                .or_else(|| config.service_tier.clone()),
        )
    };
    per_turn_config.service_tier = service_tier;

    let auto_review_enabled = crate::guardian::routes_approval_policy_to_guardian(
        per_turn_config.permissions.approval_policy.value(),
        per_turn_config.approvals_reviewer,
    );
    let per_turn_config = Arc::new(per_turn_config);
    let review_turn_id = sub_id.to_string();
    let turn_metadata_state = Arc::new(TurnMetadataState::new(
        sess.session_id().to_string(),
        sess.thread_id().to_string(),
        forked_from_thread_id,
        parent_turn_context.parent_thread_id,
        &session_source,
        thread_source,
        review_turn_id.clone(),
        #[allow(deprecated)]
        parent_turn_context.cwd.clone(),
        &parent_turn_context.permission_profile(),
        parent_turn_context.windows_sandbox_level,
        parent_turn_context.network.is_some(),
        auto_review_enabled,
        &model_info,
    ));
    if turn_metadata_state.can_start_root_turn(&session_source) {
        turn_metadata_state.set_root_turn_id(review_turn_id.clone());
    }

    let extension_data = Arc::new(codex_extension_api::ExtensionData::new(
        review_turn_id.clone(),
    ));
    extension_data.insert(parent_turn_context.skills_snapshot().as_ref().clone());

    let review_turn_context = TurnContext {
        sub_id: review_turn_id.clone(),
        trace_id: current_span_trace_id(),
        realtime_active: parent_turn_context.realtime_active,
        code_mode_available: parent_turn_context.code_mode_available,
        config: per_turn_config,
        auth_manager: auth_manager_for_context,
        model_info: model_info.clone(),
        session_telemetry: session_telemetry_for_context,
        provider: provider_for_context,
        reasoning_effort,
        reasoning_summary,
        session_source,
        history_mode: parent_turn_context.history_mode,
        parent_thread_id: parent_turn_context.parent_thread_id,
        originator: parent_turn_context.originator.clone(),
        environments: parent_turn_context.environments.clone(),
        available_models,
        unified_exec_shell_mode,
        current_date: parent_turn_context.current_date.clone(),
        timezone: parent_turn_context.timezone.clone(),
        app_server_client_name: parent_turn_context.app_server_client_name.clone(),
        developer_instructions: None,
        mode: parent_turn_context.mode,
        collaboration_mode_developer_instructions: parent_turn_context
            .collaboration_mode_developer_instructions
            .clone(),
        multi_agent_version: MultiAgentVersion::Disabled,
        personality: parent_turn_context.personality,
        network: parent_turn_context.network.clone(),
        windows_sandbox_level: parent_turn_context.windows_sandbox_level,
        #[allow(deprecated)]
        cwd: parent_turn_context.cwd.clone(),
        final_output_json_schema: None,
        dynamic_tools: parent_turn_context.dynamic_tools.clone(),
        turn_metadata_state,
        extension_data,
        turn_timing_state: Arc::new(TurnTimingState::default()),
        terminal_error: Arc::new(Mutex::new(None)),
        server_model_warning_emitted: AtomicBool::new(false),
        model_verification_emitted: AtomicBool::new(false),
    };

    // Seed the child task with the review prompt as the initial user message.
    let prompt_token_estimate = estimate_review_prompt_tokens(&review_prompt);
    let input = vec![TurnInput::UserInput {
        content: vec![UserInput::Text {
            text: review_prompt,
            // Review prompt is synthesized; no UI element ranges to preserve.
            text_elements: Vec::new(),
        }],
        client_id: None,
    }];
    let tc = Arc::new(review_turn_context);
    if tc.environments.single_local_environment_cwd().is_some() {
        tc.turn_metadata_state.spawn_git_enrichment_task();
    }
    let should_emit_review_mode = persistence.as_ref().is_none_or(|persistence| {
        matches!(
            persistence,
            ReviewPersistenceSpec::Mode(ReviewPersistence::ManualAutoReview)
        )
    });
    let manual_review_request = should_emit_review_mode.then(|| ReviewRequest {
        target: resolved.target.clone(),
        user_facing_hint: Some(resolved.user_facing_hint.clone()),
    });
    let task = if let Some(persistence) = persistence {
        let persistence = match persistence {
            ReviewPersistenceSpec::Mode(mode) => {
                let selected_cwd = tc.environments.single_local_environment_cwd();
                let target_cwd = selected_cwd
                    .as_ref()
                    .map(std::convert::AsRef::as_ref)
                    .unwrap_or_else(|| tc.config.cwd.as_ref());
                ReviewPersistenceContext::new(
                    review_turn_id,
                    mode,
                    resolved.target,
                    tc.config.codex_home.as_ref(),
                    target_cwd,
                    Some(model),
                    tc.effective_reasoning_effort()
                        .map(|effort| effort.to_string()),
                    /*prompt_token_estimate*/ None,
                )
                .await
                .with_owner_thread_id(sess.thread_id().to_string())
            }
            ReviewPersistenceSpec::Context(persistence) => *persistence,
        }
        .with_prompt_token_estimate(prompt_token_estimate);
        ReviewTask::with_persistence(persistence)
    } else {
        ReviewTask::new()
    };
    PreparedReviewThread {
        turn_context: tc,
        input,
        task,
        manual_review_request,
    }
}

async fn record_started_manual_auto_review(
    sess: &Arc<Session>,
    persistence: ReviewPersistenceContext,
) -> Option<ReviewPersistenceContext> {
    let codex_home = sess.codex_home().await;
    let coordination = ReviewCoordination::for_scope(&codex_home, persistence.store_scope());
    let mut published = None;
    let result = coordination.publish_next_snapshot_epoch_after(|snapshot_epoch| {
        let pending = persistence.clone().with_snapshot_epoch(snapshot_epoch);
        if pending.save_pending(&codex_home) {
            published = Some(pending);
            true
        } else {
            false
        }
    });
    match result {
        Ok(Some(_)) => published,
        Ok(None) => None,
        Err(err) => {
            tracing::warn!(
                run_id = %persistence.run_id(),
                error = %err,
                "failed to publish manual auto review snapshot epoch"
            );
            None
        }
    }
}

fn estimate_review_prompt_tokens(prompt: &str) -> Option<u64> {
    let bytes = u64::try_from(prompt.len()).ok()?;
    Some(
        bytes.saturating_add(ESTIMATED_BYTES_PER_REVIEW_PROMPT_TOKEN - 1)
            / ESTIMATED_BYTES_PER_REVIEW_PROMPT_TOKEN,
    )
}

pub(super) fn spawn_detached_review_thread(
    sess: Arc<Session>,
    prepared: PreparedReviewThread,
    running_review: BackgroundAutoReviewRunningHandle,
    review_lock_guard: ReviewLockGuard,
    generation: u64,
) {
    let task = Arc::new(prepared.task);
    let turn_context = prepared.turn_context;
    let input = prepared.input;
    let cancellation_token = running_review.cancellation_token;
    let completion = running_review.completion;
    tokio::spawn(async move {
        let _review_lock_guard = review_lock_guard;
        let _ = task
            .run(Arc::clone(&sess), turn_context, input, cancellation_token)
            .await;
        sess.clear_background_auto_review(generation).await;
        completion.mark_done();
    });
}

pub(super) async fn record_background_review_status(
    sess: Arc<Session>,
    persistence: &ReviewPersistenceContext,
    status: BackgroundAutoReviewStatus,
    error_summary: Option<String>,
) {
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
        sess.send_event_raw(event).await;
    })
    .await
    {
        tracing::warn!(error = %err, "background auto review status task failed");
    }
}
