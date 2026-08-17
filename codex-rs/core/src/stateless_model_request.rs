use crate::AttestationProvider;
use crate::ModelClient;
use crate::Prompt;
use crate::ResponseEvent;
use crate::config::Config;
use crate::execution_account::ExecutionAccountLease;
use crate::execution_account::ExecutionAccountLeasePersistence;
use crate::execution_account::ExecutionAccountOptions;
use crate::execution_account::ExecutionAccountPooling;
use crate::execution_account::ExecutionAccountStart;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::session_models_manager::models_manager_for_execution_account;
use codex_features::Feature;
use codex_login::AuthManager;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_models_manager::manager::SharedModelsManager;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TokenUsage;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct StatelessModelRequest {
    pub model: String,
    pub developer_instructions: String,
    pub user_input: String,
    pub output_schema: Value,
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct StatelessModelResponse {
    pub model: String,
    pub response_id: String,
    pub output: Value,
    pub token_usage: Option<TokenUsage>,
    pub duration: Duration,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_stateless_model_request(
    config: &Config,
    auth_manager: Arc<AuthManager>,
    models_manager: SharedModelsManager,
    session_source: SessionSource,
    installation_id: String,
    attestation_provider: Option<Arc<dyn AttestationProvider>>,
    request: StatelessModelRequest,
    cancellation: CancellationToken,
) -> Result<StatelessModelResponse> {
    if request.model.trim().is_empty() {
        return Err(CodexErr::InvalidRequest(
            "model must not be empty".to_string(),
        ));
    }
    if request.user_input.trim().is_empty() {
        return Err(CodexErr::InvalidRequest(
            "user input must not be empty".to_string(),
        ));
    }

    let started_at = Instant::now();
    let request_id = ThreadId::new();
    let execution_account = ExecutionAccountLease::resolve(
        request_id,
        Arc::clone(&auth_manager),
        ExecutionAccountOptions {
            codex_home: config.codex_home.to_path_buf(),
            auth_home: config.codex_home.to_path_buf(),
            auth_credentials_store_mode: config.cli_auth_credentials_store_mode,
            keyring_backend_kind: config.auth_keyring_backend_kind(),
            forced_chatgpt_workspace_id: config.forced_chatgpt_workspace_id.clone(),
            auth_route_config: config.auth_route_config(),
            chatgpt_base_url: config.chatgpt_base_url.clone(),
            allow_api_key_fallback: config.api_key_fallback_on_all_accounts_limited,
            pooling: if config.auto_switch_accounts_on_rate_limit
                && codex_login::auth::read_codex_api_key_from_env().is_none()
                && config.model_provider.requires_openai_auth
            {
                ExecutionAccountPooling::Enabled
            } else {
                ExecutionAccountPooling::Disabled
            },
            persistence: ExecutionAccountLeasePersistence::Ephemeral,
            start: ExecutionAccountStart::New,
        },
    )
    .await;
    let models_manager =
        models_manager_for_execution_account(config, execution_account.clone(), models_manager);
    let model_info = models_manager
        .get_model_info(&request.model, &config.to_models_manager_config())
        .await;
    let model_client = ModelClient::new(
        Some(auth_manager),
        if config.features.enabled(Feature::UseAgentIdentity) {
            AgentIdentityAuthPolicy::ChatGptAuth
        } else {
            AgentIdentityAuthPolicy::JwtOnly
        },
        request_id,
        config.model_provider.clone(),
        session_source.clone(),
        "codex_app_server_structured_request".to_string(),
        config.model_verbosity,
        config.features.enabled(Feature::EnableRequestCompression),
        config.features.enabled(Feature::RuntimeMetrics),
        /*beta_features_header*/ None,
        config
            .features
            .enabled(Feature::ConcurrentReasoningSummaries),
        attestation_provider,
        config.http_client_factory(),
    );
    model_client.set_execution_account_lease(execution_account);
    let session_telemetry = SessionTelemetry::new(
        request_id,
        request.model.as_str(),
        model_info.slug.as_str(),
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "codex_app_server_structured_request".to_string(),
        /*log_user_prompts*/ false,
        "app-server".to_string(),
        session_source,
    );
    let prompt = build_prompt(request.clone());
    let responses_metadata = CodexResponsesMetadata::new(
        installation_id,
        request_id.to_string(),
        request_id.to_string(),
        format!("structured-request:{request_id}"),
    );
    let inference_trace = InferenceTraceContext::disabled();
    let mut model_session = model_client.new_session();
    let mut stream = tokio::select! {
        _ = cancellation.cancelled() => return Err(CodexErr::Interrupted),
        stream = model_session.stream(
            &prompt,
            &model_info,
            &session_telemetry,
            config.model_reasoning_effort.clone(),
            ReasoningSummary::None,
            config.service_tier.clone(),
            &responses_metadata,
            &inference_trace,
        ) => stream?,
    };

    let mut streamed_text = String::new();
    let mut completed_text = None;
    let mut response_model = request.model.clone();
    let mut completion = None;
    loop {
        let event = tokio::select! {
            _ = cancellation.cancelled() => return Err(CodexErr::Interrupted),
            event = stream.next() => event,
        };
        let Some(event) = event else {
            break;
        };
        match event? {
            ResponseEvent::OutputTextDelta(delta) => streamed_text.push_str(&delta),
            ResponseEvent::OutputItemDone(ResponseItem::Message { role, content, .. })
                if role == "assistant" =>
            {
                let text = content
                    .into_iter()
                    .filter_map(|item| match item {
                        ContentItem::OutputText { text } => Some(text),
                        ContentItem::InputText { .. }
                        | ContentItem::InputImage { .. }
                        | ContentItem::InputAudio { .. } => None,
                    })
                    .collect::<String>();
                if !text.is_empty() {
                    completed_text = Some(text);
                }
            }
            ResponseEvent::OutputItemDone(ResponseItem::Reasoning { .. }) => {}
            ResponseEvent::OutputItemDone(_) | ResponseEvent::ToolCallInputDelta { .. } => {
                return Err(CodexErr::InvalidRequest(
                    "structured request returned an unexpected tool call".to_string(),
                ));
            }
            ResponseEvent::Completed {
                response_id,
                token_usage,
                ..
            } => {
                completion = Some((response_id, token_usage));
                break;
            }
            ResponseEvent::ServerModel(model) => response_model = model,
            ResponseEvent::Created
            | ResponseEvent::SafetyBuffering(_)
            | ResponseEvent::OutputItemAdded(_)
            | ResponseEvent::ModelVerifications(_)
            | ResponseEvent::TurnModerationMetadata(_)
            | ResponseEvent::ServerReasoningIncluded(_)
            | ResponseEvent::ReasoningSummaryDelta { .. }
            | ResponseEvent::ReasoningSummaryDone { .. }
            | ResponseEvent::ReasoningContentDelta { .. }
            | ResponseEvent::ReasoningSummaryPartAdded { .. }
            | ResponseEvent::RateLimits(_)
            | ResponseEvent::ModelsEtag(_) => {}
        }
    }

    let (response_id, token_usage) = completion.ok_or_else(|| {
        CodexErr::Stream("structured request stream ended before completion".to_string())
    })?;
    let output_text = completed_text.unwrap_or(streamed_text);
    let output = parse_structured_output(&output_text)?;

    Ok(StatelessModelResponse {
        model: response_model,
        response_id,
        output,
        token_usage,
        duration: started_at.elapsed(),
    })
}

fn build_prompt(request: StatelessModelRequest) -> Prompt {
    Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: request.user_input,
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        base_instructions: BaseInstructions {
            text: request.developer_instructions,
            provenance: None,
        },
        output_schema: Some(request.output_schema),
        output_schema_strict: true,
        max_output_tokens: request.max_output_tokens,
        ..Prompt::default()
    }
}

fn parse_structured_output(output_text: &str) -> Result<Value> {
    if output_text.trim().is_empty() {
        return Err(CodexErr::InvalidRequest(
            "structured request returned no assistant output".to_string(),
        ));
    }
    serde_json::from_str(output_text).map_err(|err| {
        CodexErr::InvalidRequest(format!("structured request returned invalid JSON: {err}"))
    })
}

#[cfg(test)]
#[path = "stateless_model_request_tests.rs"]
mod tests;
