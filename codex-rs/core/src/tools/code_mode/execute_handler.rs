use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_code_mode::ToolDefinition;
use codex_tools::ToolName;
use codex_tools::ToolSpec;

use super::ExecContext;
use super::PUBLIC_TOOL_NAME;
use super::handle_runtime_response;
use super::is_exec_tool_name;

pub struct CodeModeExecuteHandler {
    spec: ToolSpec,
    nested_tool_specs: Vec<ToolSpec>,
    direct_model_only_tools: Vec<ToolDefinition>,
}

impl CodeModeExecuteHandler {
    pub(crate) fn new(
        spec: ToolSpec,
        nested_tool_specs: Vec<ToolSpec>,
        direct_model_only_tools: Vec<ToolDefinition>,
    ) -> Self {
        Self {
            spec,
            nested_tool_specs,
            direct_model_only_tools,
        }
    }

    async fn execute(
        &self,
        session: std::sync::Arc<crate::session::session::Session>,
        turn: std::sync::Arc<crate::session::turn_context::TurnContext>,
        call_id: String,
        code: String,
    ) -> Result<FunctionToolOutput, FunctionCallError> {
        let args =
            codex_code_mode::parse_exec_source(&code).map_err(FunctionCallError::RespondToModel)?;
        let exec = ExecContext { session, turn };
        let enabled_tools =
            codex_tools::collect_code_mode_tool_definitions(&self.nested_tool_specs);
        let source = source_with_direct_tool_metadata(&args.code, &self.direct_model_only_tools)?;
        let started_at = std::time::Instant::now();
        let started_cell = exec
            .session
            .services
            .code_mode_service
            .execute(codex_code_mode::ExecuteRequest {
                tool_call_id: call_id.clone(),
                enabled_tools,
                source,
                yield_time_ms: args.yield_time_ms,
                max_output_tokens: args.max_output_tokens,
            })
            .await
            .map_err(FunctionCallError::RespondToModel)?;
        let cell_id = started_cell.cell_id.clone();
        if let Some(executed_tool_calls) = exec.session.services.executed_tool_calls.as_ref() {
            executed_tool_calls.register_cell(&cell_id, &call_id);
        }
        let runtime_cell_id = cell_id.to_string();
        let code_cell_trace = exec
            .session
            .services
            .rollout_thread_trace
            .start_code_cell_trace(
                exec.turn.sub_id.as_str(),
                runtime_cell_id.as_str(),
                call_id.as_str(),
                args.code.as_str(),
            );
        exec.session
            .services
            .code_mode_service
            .mark_cell_ready_for_dispatch(&cell_id);
        let response = started_cell
            .initial_response()
            .await
            .map_err(FunctionCallError::RespondToModel)?;
        // Record the raw runtime boundary. The model-visible custom-tool output
        // is produced by `handle_runtime_response` and later linked through
        // `CodeCell.output_item_ids` in the reduced trace.
        code_cell_trace.record_initial_response(&response);
        // Yielded cells keep running, so terminal lifecycle is only emitted
        // here when the first response also ended the runtime.
        if !matches!(response, codex_code_mode::RuntimeResponse::Yielded { .. }) {
            code_cell_trace.record_ended(&response);
            exec.session
                .services
                .code_mode_service
                .finish_cell_dispatch(&cell_id);
        }
        exec.session.services.elicitations.wait_until_clear().await;
        handle_runtime_response(&exec, response, args.max_output_tokens, started_at)
            .await
            .map_err(FunctionCallError::RespondToModel)
    }
}

fn source_with_direct_tool_metadata(
    source: &str,
    direct_tools: &[ToolDefinition],
) -> Result<String, FunctionCallError> {
    if direct_tools.is_empty() {
        return Ok(source.to_string());
    }
    let metadata = direct_tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "callable": false,
                "invocation": "direct",
                "recipient": super::direct_tool_recipient(&tool.tool_name),
            })
        })
        .collect::<Vec<_>>();
    let metadata = serde_json::to_string(&metadata).map_err(|error| {
        FunctionCallError::RespondToModel(format!(
            "failed to encode direct tool capability metadata: {error}"
        ))
    })?;
    Ok(format!(
        "{{ const directTools = {metadata}; for (const tool of directTools) {{ ALL_TOOLS.push(tool); }} }}\n{source}"
    ))
}

impl ToolExecutor<ToolInvocation> for CodeModeExecuteHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(PUBLIC_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn handle(&self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'_> {
        Box::pin(self.handle_call(invocation))
    }
}

impl CodeModeExecuteHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            call_id,
            tool_name,
            payload,
            ..
        } = invocation;

        match payload {
            ToolPayload::Custom { input } if is_exec_tool_name(&tool_name) => self
                .execute(session, turn, call_id, input)
                .await
                .map(boxed_tool_output),
            _ => Err(FunctionCallError::RespondToModel(format!(
                "{PUBLIC_TOOL_NAME} expects raw JavaScript source text"
            ))),
        }
    }
}

impl CoreToolRuntime for CodeModeExecuteHandler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Custom { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_tool_metadata_leaves_source_unchanged_when_empty() {
        let source = "text('ready');";

        assert_eq!(
            source_with_direct_tool_metadata(source, &[]).expect("source should encode"),
            source
        );
    }

    #[test]
    fn direct_tool_metadata_is_machine_readable_and_not_callable() {
        let source = "text('ready');";
        let tool = ToolDefinition {
            name: "agents__spawn_agent".to_string(),
            tool_name: ToolName::namespaced("agents", "spawn_agent"),
            description: "Call the direct tool.".to_string(),
            kind: codex_code_mode::CodeModeToolKind::Function,
            input_schema: None,
            output_schema: None,
        };

        let encoded =
            source_with_direct_tool_metadata(source, &[tool]).expect("source should encode");
        let metadata = encoded
            .strip_prefix("{ const directTools = ")
            .and_then(|encoded| encoded.split_once("; for (const tool of directTools)"))
            .map(|(metadata, _)| metadata)
            .expect("metadata prelude should be present");
        let metadata: serde_json::Value =
            serde_json::from_str(metadata).expect("metadata should be valid JSON");

        assert_eq!(
            metadata,
            serde_json::json!([{
                "name": "agents__spawn_agent",
                "description": "Call the direct tool.",
                "callable": false,
                "invocation": "direct",
                "recipient": "functions.agents.spawn_agent",
            }])
        );
        assert!(encoded.ends_with(source));
        assert!(!encoded.contains("Object.defineProperty"));
    }
}
