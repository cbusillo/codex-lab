use super::*;
use crate::agent::control::SpawnAgentForkMode;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::next_thread_spawn_depth;
use crate::agent::provider_routing::AgentTaskKind;
use crate::agent::provider_routing::AgentTaskSize;
use crate::agent::provider_routing::ProviderRoutingSummary;
use crate::agent::provider_routing::select_provider_route;
use crate::agent::user_agent_intent::UserAgentIntent;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::tools::handlers::multi_agents_spec::SpawnAgentToolOptions;
use crate::tools::handlers::multi_agents_spec::create_spawn_agent_tool_v2;
use crate::tools::handlers::multi_agents_v2::message_tool::message_content;
use codex_config::agent_defaults::agent_model_spec;
use codex_protocol::AgentPath;
use codex_tools::ToolSpec;

#[derive(Default)]
pub(crate) struct Handler {
    options: SpawnAgentToolOptions,
}

impl Handler {
    pub(crate) fn new(options: SpawnAgentToolOptions) -> Self {
        Self { options }
    }
}

impl ToolExecutor<ToolInvocation> for Handler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("spawn_agent")
    }

    fn spec(&self) -> ToolSpec {
        create_spawn_agent_tool_v2(self.options.clone())
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(async move { handle_spawn_agent(invocation).await.map(boxed_tool_output) })
    }
}

async fn handle_spawn_agent(
    invocation: ToolInvocation,
) -> Result<SpawnAgentResult, FunctionCallError> {
    let ToolInvocation {
        session,
        step_context,
        payload,
        call_id,
        source,
        ..
    } = invocation;
    let turn = &step_context.turn;
    let arguments = function_arguments(payload)?;
    let args: SpawnAgentArgs = parse_arguments(&arguments)?;
    let selectors = resolve_spawn_selectors(args.agent_type.as_deref(), args.model.as_deref())?;
    enforce_explicit_user_agent_intent(turn, &selectors)?;
    let requested_role_name = selectors.agent_type.as_deref();
    if let Some(role_name) = requested_role_name
        && !crate::agent::role::agent_selector_enabled(&turn.config, role_name)
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "agent_type `{role_name}` is disabled by configuration"
        )));
    }

    let message = message_content(args.message.clone())?;
    let session_source = turn.session_source.clone();
    let child_depth = next_thread_spawn_depth(&session_source);
    let mut config =
        build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())?;
    apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;
    crate::agent::selector_defaults::install_configured_provider_selectors(&mut config)
        .map_err(FunctionCallError::RespondToModel)?;
    let explicit_role_name = requested_role_name.map(|role_name| {
        crate::agent::selector_defaults::resolve_effective_selector(&config, role_name)
    });
    let explicit_effort = args.reasoning_effort.as_ref().map(ToString::to_string);
    if let Some(role_name) = explicit_role_name.as_deref() {
        crate::agent::selector_defaults::install_selected_provider_defaults(
            &mut config,
            role_name,
            explicit_effort.as_deref(),
        )
        .map_err(FunctionCallError::RespondToModel)?;
    }
    let routing = select_provider_route(
        &config,
        explicit_role_name.as_deref(),
        args.task_kind,
        args.task_size,
    )
    .await
    .map_err(|failure| FunctionCallError::RespondToModel(failure.message()))?;
    enforce_routed_user_agent_intent(turn, routing.agent_type())?;
    let role_name = routing.role_name();
    let preflighted_external_role = if routing.is_external() {
        role_name.and_then(|role_name| {
            crate::agent::role::resolve_role_config_owned(&config, role_name)
                .map(|role| (role_name.to_string(), role))
        })
    } else {
        None
    };
    let fork_mode = args.fork_mode(routing.is_external())?;
    if routing.is_external() && fork_mode.is_some() {
        return Err(FunctionCallError::RespondToModel(
            "External agents do not support fork_turns; use `fork_turns = \"none\"` or omit it when an external agent is selected."
                .to_string(),
        ));
    }
    if let Some(service_tier) = args.service_tier.as_ref() {
        config.service_tier = Some(service_tier.clone());
    }
    let is_full_history_fork = matches!(fork_mode, Some(SpawnAgentForkMode::FullHistory));
    if is_full_history_fork {
        reject_full_fork_agent_type_override(role_name)?;
    }
    if !routing.is_external() {
        apply_requested_spawn_agent_model_overrides(
            &session,
            turn.as_ref(),
            &mut config,
            selectors.model.as_deref(),
            args.reasoning_effort.clone(),
        )
        .await?;
    }
    if !is_full_history_fork {
        apply_spawn_agent_role(&session, &mut config, role_name).await?;
    }
    if let Some((role_name, role)) = preflighted_external_role {
        config.agent_roles.insert(role_name, role);
    }
    apply_spawn_agent_service_tier(
        &session,
        &mut config,
        turn.config.service_tier.as_deref(),
        args.service_tier.as_deref(),
    )
    .await?;
    apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;

    let spawn_source = thread_spawn_source(
        session.thread_id,
        &turn.session_source,
        child_depth,
        role_name,
        Some(args.task_name.clone()),
    )?;
    let new_agent_path = spawn_source.get_agent_path().ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "spawned agent is missing a canonical task name".to_string(),
        )
    })?;
    let author = turn
        .session_source
        .get_agent_path()
        .unwrap_or_else(AgentPath::root);
    let communication = communication_from_tool_message(
        author,
        new_agent_path.clone(),
        message,
        &source,
        /*trigger_turn*/ true,
    );
    let context = AgentCommunicationContext::new(AgentCommunicationKind::Spawn, session.thread_id);
    let spawned_agent = Box::pin(
        session
            .services
            .agent_control
            .spawn_agent_with_communication(
                config,
                communication,
                context,
                Some(spawn_source),
                SpawnAgentOptions {
                    fork_parent_spawn_call_id: fork_mode.as_ref().map(|_| call_id.clone()),
                    fork_mode,
                    parent_thread_id: Some(session.thread_id),
                    parent_turn_id: Some(turn.sub_id.clone()),
                    environments: Some(step_context.environments.to_selections()),
                    external_agent_provider: routing.provider().cloned(),
                    external_agent_routing: Some(routing.summary()),
                },
            ),
    )
    .await
    .map_err(collab_spawn_error)?;
    let new_thread_id = spawned_agent.thread_id;
    let agent_snapshot = session
        .services
        .agent_control
        .get_agent_config_snapshot(new_thread_id)
        .await;
    let nickname = agent_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.session_source.get_nickname())
        .or(spawned_agent.metadata.agent_nickname);
    emit_sub_agent_activity(
        &session,
        turn,
        SubAgentActivityItem {
            id: call_id,
            agent_thread_id: new_thread_id,
            agent_path: new_agent_path.clone(),
            kind: SubAgentActivityKind::Started,
        },
    )
    .await;
    turn.session_telemetry.counter(
        "codex.multi_agent.spawn",
        /*inc*/ 1,
        &[
            ("role", routing.agent_type()),
            ("routing", routing.kind().as_str()),
            ("task_kind", args.task_kind.as_str()),
            ("task_size", args.task_size.as_str()),
            ("version", "v2"),
        ],
    );
    let task_name = String::from(new_agent_path);

    let hide_agent_metadata = turn.config.multi_agent_v2.hide_spawn_agent_metadata;
    if hide_agent_metadata {
        Ok(SpawnAgentResult::HiddenMetadata {
            task_name,
            routing: routing.redacted_summary(),
        })
    } else {
        Ok(SpawnAgentResult::WithNickname {
            task_name,
            nickname,
            agent_type: routing.agent_type().to_string(),
            routing: routing.summary(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedSpawnSelectors {
    agent_type: Option<String>,
    model: Option<String>,
}

fn enforce_explicit_user_agent_intent(
    turn: &crate::session::turn_context::TurnContext,
    selectors: &ResolvedSpawnSelectors,
) -> Result<(), FunctionCallError> {
    let Some(intent) = turn.extension_data.get::<UserAgentIntent>() else {
        return Ok(());
    };
    if intent.is_empty() {
        return Ok(());
    }
    let required = intent
        .required()
        .iter()
        .map(|slug| format!("`{slug}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let selected = selectors
        .agent_type
        .as_deref()
        .or(selectors.model.as_deref());
    if let Some(selected) = selected
        && intent
            .rejected()
            .iter()
            .any(|rejected| intent_selector_matches(rejected, selected))
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "The current user turn explicitly rejects `{selected}`. Do not spawn that agent; choose an allowed alternative."
        )));
    }
    if intent.required().is_empty() {
        return Ok(());
    }
    let Some(selected) = selected else {
        return Err(FunctionCallError::RespondToModel(format!(
            "The current user turn explicitly requests specific agents: {required}. Set `agent_type` to one of those canonical selectors before spawning. Do not use automatic routing for a named-agent request."
        )));
    };
    if intent
        .required()
        .iter()
        .any(|required| intent_selector_matches(required, selected))
    {
        return Ok(());
    }
    Err(FunctionCallError::RespondToModel(format!(
        "The current user turn explicitly requests specific agents: {required}, but the spawn selected `{selected}`. Use one of the requested canonical selectors and do not substitute another agent."
    )))
}

fn enforce_routed_user_agent_intent(
    turn: &crate::session::turn_context::TurnContext,
    selected: &str,
) -> Result<(), FunctionCallError> {
    let Some(intent) = turn.extension_data.get::<UserAgentIntent>() else {
        return Ok(());
    };
    validate_routed_user_agent_intent(intent.as_ref(), selected)
}

fn validate_routed_user_agent_intent(
    intent: &UserAgentIntent,
    selected: &str,
) -> Result<(), FunctionCallError> {
    if intent
        .rejected()
        .iter()
        .any(|rejected| intent_selector_matches(rejected, selected))
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "Automatic routing selected `{selected}`, but the current user turn explicitly rejects that agent. Retry with an allowed explicit `agent_type`."
        )));
    }
    if intent.required().is_empty()
        || intent
            .required()
            .iter()
            .any(|required| intent_selector_matches(required, selected))
    {
        return Ok(());
    }
    let required = intent
        .required()
        .iter()
        .map(|slug| format!("`{slug}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(FunctionCallError::RespondToModel(format!(
        "Routing selected `{selected}`, but the current user turn explicitly requests {required}. Retry with one of the requested canonical selectors."
    )))
}

fn intent_selector_matches(intent_selector: &str, selected: &str) -> bool {
    intent_selector == selected
        || (intent_selector == "antigravity"
            && crate::agent::external_capabilities::looks_like_antigravity_selector(selected))
}

#[cfg(test)]
mod intent_tests {
    use super::*;
    use codex_protocol::user_input::UserInput;

    fn intent(text: &str) -> UserAgentIntent {
        UserAgentIntent::from_user_input(&[UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }])
    }

    #[test]
    fn rejection_only_intent_blocks_automatic_route_result() {
        let intent = intent("Do not use Opus.");

        assert!(validate_routed_user_agent_intent(&intent, "claude-opus-5").is_err());
        assert!(validate_routed_user_agent_intent(&intent, "claude-sonnet-4.6").is_ok());
    }

    #[test]
    fn required_intent_blocks_a_different_automatic_route_result() {
        let intent = intent("Ask Opus to review this.");

        assert!(validate_routed_user_agent_intent(&intent, "claude-sonnet-4.6").is_err());
        assert!(validate_routed_user_agent_intent(&intent, "claude-opus-5").is_ok());
    }

    #[test]
    fn provider_intent_accepts_exact_antigravity_model_without_accepting_other_models() {
        let intent = intent("Ask Antigravity to review this.");

        assert!(
            validate_routed_user_agent_intent(&intent, "antigravity-gemini-3.6-flash-high").is_ok()
        );
        assert!(
            validate_routed_user_agent_intent(&intent, "antigravity-gemini-3.1-pro-low").is_ok()
        );
        assert!(validate_routed_user_agent_intent(&intent, "claude-sonnet-4.6").is_err());
    }
}

fn resolve_spawn_selectors(
    agent_type: Option<&str>,
    model: Option<&str>,
) -> Result<ResolvedSpawnSelectors, FunctionCallError> {
    let agent_type = agent_type.map(str::trim).filter(|role| !role.is_empty());
    let model = model.map(str::trim).filter(|model| !model.is_empty());
    let agent_type_selector = agent_type.and_then(canonical_external_selector);
    let model_selector = model.and_then(canonical_external_selector);
    match (agent_type, agent_type_selector, model, model_selector) {
        (None, _, Some(_), Some(model_selector)) => Ok(ResolvedSpawnSelectors {
            agent_type: Some(model_selector),
            model: None,
        }),
        (Some(_), Some(agent_type_selector), Some(_), Some(model_selector))
            if agent_type_selector == model_selector =>
        {
            Ok(ResolvedSpawnSelectors {
                agent_type: Some(agent_type_selector),
                model: None,
            })
        }
        (Some(agent_type), Some(agent_type_selector), Some(model), Some(model_selector)) => {
            Err(FunctionCallError::RespondToModel(format!(
                "external agent selector `{model}` resolves to `{model_selector}`, but agent type `{agent_type}` resolves to `{agent_type_selector}`; use one explicit agent selector"
            )))
        }
        (Some(agent_type), Some(_), Some(model), None) => {
            Err(FunctionCallError::RespondToModel(format!(
                "external agent type `{agent_type}` cannot be combined with native model override `{model}`; use one explicit agent selector"
            )))
        }
        (Some(agent_type), None, Some(model), Some(model_selector)) => {
            Err(FunctionCallError::RespondToModel(format!(
                "external agent selector `{model}` resolves to `{model_selector}`, but agent type `{agent_type}` selects a different role; use one explicit agent selector"
            )))
        }
        (Some(_), Some(agent_type_selector), None, _) => Ok(ResolvedSpawnSelectors {
            agent_type: Some(agent_type_selector),
            model: None,
        }),
        _ => Ok(ResolvedSpawnSelectors {
            agent_type: agent_type.map(str::to_string),
            model: model.map(str::to_string),
        }),
    }
}

fn canonical_external_selector(selector: &str) -> Option<String> {
    // Keep unknown provider-qualified selectors on the external route so bounded
    // preflight can reject them with the installed CLI's actionable capability
    // diagnostics. Never fall back to a native or provider-default selector.
    if crate::agent::external_capabilities::looks_like_antigravity_selector(selector) {
        return Some(selector.to_string());
    }
    agent_model_spec(selector)
        .filter(|spec| spec.family != "code" && spec.is_enabled())
        .map(|spec| spec.slug.to_string())
}

impl CoreToolRuntime for Handler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentArgs {
    message: String,
    task_name: String,
    agent_type: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
    service_tier: Option<String>,
    #[serde(default)]
    task_kind: AgentTaskKind,
    #[serde(default)]
    task_size: AgentTaskSize,
    fork_turns: Option<String>,
    fork_context: Option<bool>,
}

impl SpawnAgentArgs {
    fn fork_mode(
        &self,
        default_to_no_fork: bool,
    ) -> Result<Option<SpawnAgentForkMode>, FunctionCallError> {
        if self.fork_context.is_some() {
            return Err(FunctionCallError::RespondToModel(
                "fork_context is not supported in MultiAgentV2; use fork_turns instead".to_string(),
            ));
        }

        let fork_turns = self
            .fork_turns
            .as_deref()
            .map(str::trim)
            .filter(|fork_turns| !fork_turns.is_empty())
            .unwrap_or(if default_to_no_fork { "none" } else { "all" });

        if fork_turns.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        if fork_turns.eq_ignore_ascii_case("all") {
            return Ok(Some(SpawnAgentForkMode::FullHistory));
        }

        let last_n_turns = fork_turns.parse::<usize>().map_err(|_| {
            FunctionCallError::RespondToModel(
                "fork_turns must be `none`, `all`, or a positive integer string".to_string(),
            )
        })?;
        if last_n_turns == 0 {
            return Err(FunctionCallError::RespondToModel(
                "fork_turns must be `none`, `all`, or a positive integer string".to_string(),
            ));
        }

        Ok(Some(SpawnAgentForkMode::LastNTurns(last_n_turns)))
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum SpawnAgentResult {
    WithNickname {
        task_name: String,
        nickname: Option<String>,
        agent_type: String,
        routing: ProviderRoutingSummary,
    },
    HiddenMetadata {
        task_name: String,
        routing: ProviderRoutingSummary,
    },
}

impl ToolOutput for SpawnAgentResult {
    fn log_preview(&self) -> String {
        tool_output_json_text(self, "spawn_agent")
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        tool_output_response_item(call_id, payload, self, Some(true), "spawn_agent")
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> JsonValue {
        tool_output_code_mode_result(self, "spawn_agent")
    }
}
