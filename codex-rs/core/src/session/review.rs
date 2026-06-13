use super::*;
use codex_core_skills::HostLoadedSkills;
use codex_protocol::openai_models::ToolMode;
use std::sync::atomic::AtomicBool;

use crate::state::BackgroundAutoReviewRunningHandle;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskContext;

pub(super) struct PreparedReviewThread {
    pub(super) turn_context: Arc<TurnContext>,
    pub(super) input: Vec<TurnInput>,
    pub(super) task: ReviewTask,
    manual_review_request: Option<ReviewRequest>,
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
    let prepared = prepare_review_thread(
        Arc::clone(&sess),
        config,
        parent_turn_context,
        sub_id,
        resolved,
        persistence,
    )
    .await;

    let manual_review_request = prepared.manual_review_request.clone();
    if let Some(review_request) = manual_review_request {
        sess.abort_all_tasks(TurnAbortReason::Replaced).await;
        sess.clear_connector_selection().await;
        sess.send_event(
            prepared.turn_context.as_ref(),
            EventMsg::EnteredReviewMode(review_request),
        )
        .await;
        sess.start_task(prepared.turn_context, prepared.input, prepared.task)
            .await;
    } else {
        sess.spawn_task(
            Arc::clone(&prepared.turn_context),
            prepared.input,
            prepared.task,
        )
        .await;
    }
}

pub(super) async fn prepare_review_thread(
    sess: Arc<Session>,
    config: Arc<Config>,
    parent_turn_context: Arc<TurnContext>,
    sub_id: String,
    resolved: crate::review_prompts::ResolvedReviewRequest,
    persistence: Option<ReviewPersistence>,
) -> PreparedReviewThread {
    let model = config
        .review_model
        .clone()
        .unwrap_or_else(|| parent_turn_context.model_info.slug.clone());
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
    let available_models = sess
        .services
        .models_manager
        .list_models(RefreshStrategy::OnlineIfUncached)
        .await;
    let unified_exec_shell_mode = UnifiedExecShellMode::for_session(
        codex_tools::unified_exec_feature_mode_for_features(review_features.get()),
        crate::tools::tool_user_shell_type(sess.services.user_shell.as_ref()),
        sess.services.shell_zsh_path.as_ref(),
        sess.services.main_execve_wrapper_exe.as_ref(),
    );

    let review_prompt = resolved.prompt.clone();
    let provider = parent_turn_context.provider.clone();
    let auth_manager = parent_turn_context.auth_manager.clone();
    let model_info = review_model_info.clone();

    // Build per‑turn client with the requested model/family.
    let mut per_turn_config = (*config).clone();
    per_turn_config.model = Some(model.clone());
    per_turn_config.features = review_features.clone();
    let tool_mode = model_info.tool_mode.unwrap_or_else(|| {
        if per_turn_config.features.enabled(Feature::CodeModeOnly) {
            ToolMode::CodeModeOnly
        } else if per_turn_config.features.enabled(Feature::CodeMode) {
            ToolMode::CodeMode
        } else {
            ToolMode::Direct
        }
    });
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
    let forked_from_thread_id = {
        let state = sess.state.lock().await;
        state.session_configuration.forked_from_thread_id
    };

    let per_turn_config = Arc::new(per_turn_config);
    let review_turn_id = sub_id.to_string();
    let turn_metadata_state = Arc::new(TurnMetadataState::new(
        sess.session_id().to_string(),
        sess.thread_id().to_string(),
        forked_from_thread_id,
        parent_turn_context.parent_thread_id,
        &session_source,
        parent_turn_context.thread_source,
        review_turn_id.clone(),
        #[allow(deprecated)]
        parent_turn_context.cwd.clone(),
        &parent_turn_context.permission_profile,
        parent_turn_context.windows_sandbox_level,
        parent_turn_context.network.is_some(),
    ));

    let extension_data = Arc::new(codex_extension_api::ExtensionData::new(
        review_turn_id.clone(),
    ));
    extension_data.insert(HostLoadedSkills::new(
        parent_turn_context.turn_skills.outcome.clone(),
    ));

    let review_turn_context = TurnContext {
        sub_id: review_turn_id.clone(),
        trace_id: current_span_trace_id(),
        realtime_active: parent_turn_context.realtime_active,
        config: per_turn_config,
        auth_manager: auth_manager_for_context,
        model_info: model_info.clone(),
        tool_mode,
        session_telemetry: session_telemetry_for_context,
        provider: provider_for_context,
        reasoning_effort,
        reasoning_summary,
        session_source,
        parent_thread_id: parent_turn_context.parent_thread_id,
        thread_source: parent_turn_context.thread_source,
        environments: parent_turn_context.environments.clone(),
        available_models,
        unified_exec_shell_mode,
        features: review_features,
        ghost_snapshot: parent_turn_context.ghost_snapshot.clone(),
        current_date: parent_turn_context.current_date.clone(),
        timezone: parent_turn_context.timezone.clone(),
        app_server_client_name: parent_turn_context.app_server_client_name.clone(),
        developer_instructions: None,
        user_instructions: None,
        compact_prompt: parent_turn_context.compact_prompt.clone(),
        collaboration_mode: parent_turn_context.collaboration_mode.clone(),
        multi_agent_version: MultiAgentVersion::Disabled,
        personality: parent_turn_context.personality,
        approval_policy: parent_turn_context.approval_policy.clone(),
        permission_profile: parent_turn_context.permission_profile(),
        network: parent_turn_context.network.clone(),
        windows_sandbox_level: parent_turn_context.windows_sandbox_level,
        shell_environment_policy: parent_turn_context.shell_environment_policy.clone(),
        #[allow(deprecated)]
        cwd: parent_turn_context.cwd.clone(),
        final_output_json_schema: None,
        codex_self_exe: parent_turn_context.codex_self_exe.clone(),
        codex_linux_sandbox_exe: parent_turn_context.codex_linux_sandbox_exe.clone(),
        dynamic_tools: parent_turn_context.dynamic_tools.clone(),
        truncation_policy: model_info.truncation_policy.into(),
        turn_metadata_state,
        extension_data,
        turn_skills: TurnSkillsContext::new(parent_turn_context.turn_skills.outcome.clone()),
        turn_timing_state: Arc::new(TurnTimingState::default()),
        server_model_warning_emitted: AtomicBool::new(false),
        model_verification_emitted: AtomicBool::new(false),
    };

    // Seed the child task with the review prompt as the initial user message.
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
    // TODO(ccunningham): Review turns currently rely on `spawn_task` for TurnComplete but do not
    // emit a parent TurnStarted. Consider giving review a full parent turn lifecycle
    // (TurnStarted + TurnComplete) for consistency with other standalone tasks.
    let should_emit_review_mode =
        persistence.is_none_or(|mode| matches!(mode, ReviewPersistence::ManualAutoReview));
    let manual_review_request = if should_emit_review_mode {
        Some(ReviewRequest {
            target: resolved.target.clone(),
            user_facing_hint: Some(resolved.user_facing_hint.clone()),
        })
    } else {
        None
    };
    if let Some(persistence) = persistence {
        let target_cwd = tc
            .environments
            .single_local_environment_cwd()
            .map(std::convert::AsRef::as_ref)
            .unwrap_or_else(|| tc.config.cwd.as_ref());
        let persistence = crate::review_persistence::ReviewPersistenceContext::new(
            review_turn_id.clone(),
            persistence,
            resolved.target.clone(),
            target_cwd,
            Some(model),
        )
        .await;
        PreparedReviewThread {
            turn_context: tc,
            input,
            task: ReviewTask::with_persistence(persistence),
            manual_review_request,
        }
    } else {
        PreparedReviewThread {
            turn_context: tc,
            input,
            task: ReviewTask::new(),
            manual_review_request,
        }
    }
}

pub(super) fn spawn_detached_review_thread(
    sess: Arc<Session>,
    prepared: PreparedReviewThread,
    running_review: BackgroundAutoReviewRunningHandle,
    generation: u64,
) {
    let turn_extension_data = Arc::clone(&prepared.turn_context.extension_data);
    let session_ctx = Arc::new(SessionTaskContext::new(
        Arc::clone(&sess),
        turn_extension_data,
    ));
    let task = Arc::new(prepared.task);
    let turn_context = prepared.turn_context;
    let input = prepared.input;
    let cancellation_token = running_review.cancellation_token;
    let completion = running_review.completion;
    tokio::spawn(async move {
        task.run(session_ctx, turn_context, input, cancellation_token)
            .await;
        sess.clear_background_auto_review(generation).await;
        completion.mark_done();
    });
}
