use crate::config_manager::ConfigManager;
use crate::error_code::internal_error;
use crate::error_code::invalid_params;
use crate::outgoing_message::ConnectionId;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::StructuredRequestCancelParams;
use codex_app_server_protocol::StructuredRequestCancelResponse;
use codex_app_server_protocol::StructuredRequestStartParams;
use codex_app_server_protocol::StructuredRequestStartResponse;
use codex_app_server_protocol::StructuredRequestTokenUsage;
use codex_core::StatelessModelRequest;
use codex_core::ThreadManager;
use codex_protocol::error::CodexErrorDetails;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct StructuredRequestProcessor {
    thread_manager: Arc<ThreadManager>,
    config_manager: ConfigManager,
    active_requests: Arc<Mutex<HashMap<ActiveRequestKey, CancellationToken>>>,
}

impl StructuredRequestProcessor {
    pub(crate) fn new(thread_manager: Arc<ThreadManager>, config_manager: ConfigManager) -> Self {
        Self {
            thread_manager,
            config_manager,
            active_requests: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn start(
        &self,
        connection_id: ConnectionId,
        params: StructuredRequestStartParams,
    ) -> Result<StructuredRequestStartResponse, JSONRPCErrorError> {
        let request_id = params.request_id.clone();
        if request_id.trim().is_empty() {
            return Err(invalid_params("requestId must not be empty"));
        }
        if params.timeout_ms == Some(0) {
            return Err(invalid_params("timeoutMs must be greater than zero"));
        }

        let cancellation = CancellationToken::new();
        let request_key = ActiveRequestKey {
            connection_id,
            request_id: request_id.clone(),
        };
        let _registration =
            register_active_request(&self.active_requests, request_key, cancellation.clone())?;

        let future = async {
            let request = StatelessModelRequest {
                model: params.model,
                developer_instructions: params.developer_instructions,
                user_input: params.user_input,
                output_schema: params.output_schema,
                max_output_tokens: params.max_output_tokens,
            };
            let execution = async {
                let config = self
                    .config_manager
                    .load_latest_config(/*fallback_cwd*/ None)
                    .await
                    .map_err(|err| internal_error(format!("failed to load config: {err}")))?;
                self.thread_manager
                    .run_stateless_model_request(&config, request, cancellation.clone())
                    .await
                    .map_err(structured_request_error)
            };
            tokio::select! {
                _ = cancellation.cancelled() => Err(internal_error("structured request was cancelled")),
                result = execution => result,
            }
        };
        let response = match params.timeout_ms {
            Some(timeout_ms) => {
                match tokio::time::timeout(Duration::from_millis(timeout_ms), future).await {
                    Ok(result) => result,
                    Err(_) => {
                        cancellation.cancel();
                        return Err(internal_error(format!(
                            "structured request `{request_id}` timed out after {timeout_ms} ms"
                        )));
                    }
                }
            }
            None => future.await,
        }?;

        Ok(StructuredRequestStartResponse {
            request_id,
            model: response.model,
            response_id: response.response_id,
            output: response.output,
            usage: response
                .token_usage
                .map(|usage| StructuredRequestTokenUsage {
                    input_tokens: usage.input_tokens,
                    cached_input_tokens: usage.cached_input_tokens,
                    cache_write_input_tokens: usage.cache_write_input_tokens,
                    output_tokens: usage.output_tokens,
                    reasoning_output_tokens: usage.reasoning_output_tokens,
                    total_tokens: usage.total_tokens,
                }),
            duration_ms: u64::try_from(response.duration.as_millis()).unwrap_or(u64::MAX),
        })
    }

    pub(crate) fn cancel(
        &self,
        connection_id: ConnectionId,
        params: StructuredRequestCancelParams,
    ) -> StructuredRequestCancelResponse {
        let request_key = ActiveRequestKey {
            connection_id,
            request_id: params.request_id,
        };
        StructuredRequestCancelResponse {
            cancelled: cancel_active_request(&self.active_requests, &request_key),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ActiveRequestKey {
    connection_id: ConnectionId,
    request_id: String,
}

fn register_active_request(
    active_requests: &Arc<Mutex<HashMap<ActiveRequestKey, CancellationToken>>>,
    request_key: ActiveRequestKey,
    cancellation: CancellationToken,
) -> Result<ActiveRequestRegistration, JSONRPCErrorError> {
    let mut requests = active_requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if requests.contains_key(&request_key) {
        return Err(invalid_params(format!(
            "structured request `{}` is already active",
            request_key.request_id
        )));
    }
    requests.insert(request_key.clone(), cancellation);
    drop(requests);
    Ok(ActiveRequestRegistration {
        active_requests: Arc::clone(active_requests),
        request_key,
    })
}

fn cancel_active_request(
    active_requests: &Arc<Mutex<HashMap<ActiveRequestKey, CancellationToken>>>,
    request_key: &ActiveRequestKey,
) -> bool {
    let cancellation = active_requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(request_key)
        .cloned();
    if let Some(cancellation) = cancellation {
        cancellation.cancel();
        true
    } else {
        false
    }
}

struct ActiveRequestRegistration {
    active_requests: Arc<Mutex<HashMap<ActiveRequestKey, CancellationToken>>>,
    request_key: ActiveRequestKey,
}

impl Drop for ActiveRequestRegistration {
    fn drop(&mut self) {
        self.active_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.request_key);
    }
}

fn structured_request_error(error: codex_protocol::error::CodexErr) -> JSONRPCErrorError {
    match error.details() {
        CodexErrorDetails::InvalidRequest(message) => invalid_params(message.clone()),
        CodexErrorDetails::Interrupted => internal_error("structured request was cancelled"),
        _ => internal_error(format!("structured request failed: {error}")),
    }
}

#[cfg(test)]
#[path = "structured_request_processor_tests.rs"]
mod tests;
