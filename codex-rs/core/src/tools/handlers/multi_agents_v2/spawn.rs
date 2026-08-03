use super::*;
use crate::agent::control::SpawnAgentForkMode;
use crate::agent::control::SpawnAgentOptions;
use crate::agent::next_thread_spawn_depth;
use crate::agent::provider_routing::AgentTaskKind;
use crate::agent::provider_routing::AgentTaskSize;
use crate::agent::provider_routing::ProviderRoutingSummary;
use crate::agent::provider_routing::select_provider_route;
use crate::agent_communication::AgentCommunicationContext;
use crate::agent_communication::AgentCommunicationKind;
use crate::tools::handlers::multi_agents_spec::SpawnAgentToolOptions;
use crate::tools::handlers::multi_agents_spec::create_spawn_agent_tool_v2;
use crate::tools::handlers::multi_agents_v2::message_tool::message_content;
use codex_config::agent_defaults::AgentModelSpec;
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
    let explicit_role_name = selectors.agent_type.as_deref();

    let message = message_content(args.message.clone())?;
    let session_source = turn.session_source.clone();
    let child_depth = next_thread_spawn_depth(&session_source);
    let mut config =
        build_agent_spawn_config(&session.get_base_instructions().await, turn.as_ref())?;
    apply_spawn_agent_runtime_overrides(&mut config, turn.as_ref())?;
    let routing =
        select_provider_route(&config, explicit_role_name, args.task_kind, args.task_size)
            .await
            .map_err(|failure| FunctionCallError::RespondToModel(failure.message()))?;
    let role_name = routing.role_name();
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
    apply_requested_spawn_agent_model_overrides(
        &session,
        turn.as_ref(),
        &mut config,
        selectors.model.as_deref(),
        args.reasoning_effort.clone(),
    )
    .await?;
    if !is_full_history_fork {
        apply_spawn_agent_role(&session, &mut config, role_name).await?;
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

fn resolve_spawn_selectors(
    agent_type: Option<&str>,
    model: Option<&str>,
) -> Result<ResolvedSpawnSelectors, FunctionCallError> {
    let agent_type = agent_type.map(str::trim).filter(|role| !role.is_empty());
    let model = model.map(str::trim).filter(|model| !model.is_empty());
    let agent_type_selector = agent_type.and_then(external_agent_spec);
    let model_selector = model.and_then(external_agent_spec);
    match (agent_type, agent_type_selector, model, model_selector) {
        (None, _, Some(_), Some(model_selector)) => Ok(ResolvedSpawnSelectors {
            agent_type: Some(model_selector.slug.to_string()),
            model: None,
        }),
        (Some(_), Some(agent_type_selector), Some(_), Some(model_selector))
            if agent_type_selector.slug == model_selector.slug =>
        {
            Ok(ResolvedSpawnSelectors {
                agent_type: Some(agent_type_selector.slug.to_string()),
                model: None,
            })
        }
        (Some(agent_type), Some(agent_type_selector), Some(model), Some(model_selector)) => {
            Err(FunctionCallError::RespondToModel(format!(
                "external agent selector `{model}` resolves to `{}`, but agent type `{agent_type}` resolves to `{}`; use one explicit agent selector",
                model_selector.slug, agent_type_selector.slug
            )))
        }
        (Some(agent_type), Some(_), Some(model), None) => {
            Err(FunctionCallError::RespondToModel(format!(
                "external agent type `{agent_type}` cannot be combined with native model override `{model}`; use one explicit agent selector"
            )))
        }
        (Some(agent_type), None, Some(model), Some(model_selector)) => {
            Err(FunctionCallError::RespondToModel(format!(
                "external agent selector `{model}` resolves to `{}`, but agent type `{agent_type}` selects a different role; use one explicit agent selector",
                model_selector.slug
            )))
        }
        (Some(_), Some(agent_type_selector), None, _) => Ok(ResolvedSpawnSelectors {
            agent_type: Some(agent_type_selector.slug.to_string()),
            model: None,
        }),
        _ => Ok(ResolvedSpawnSelectors {
            agent_type: agent_type.map(str::to_string),
            model: model.map(str::to_string),
        }),
    }
}

fn external_agent_spec(selector: &str) -> Option<&'static AgentModelSpec> {
    agent_model_spec(selector).filter(|spec| spec.family != "code" && spec.is_enabled())
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
