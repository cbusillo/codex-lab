use codex_auto_review::AutoReviewDispositionActor;
use codex_auto_review::AutoReviewFindingDisposition;
use codex_auto_review::AutoReviewFindingDispositionRecord;
use codex_auto_review::AutoReviewStore;
use codex_protocol::protocol::ReviewTarget;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use crate::function_tool::FunctionCallError;
use crate::review_persistence::collect_auto_review_target;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::auto_review_disposition_spec::AUTO_REVIEW_DISPOSITION_TOOL_NAME;
use crate::tools::handlers::auto_review_disposition_spec::create_auto_review_disposition_tool;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use crate::turn_timing::now_unix_timestamp_ms;

const AUTO_REVIEW_REPAIR_RESPONSE_MAX_BYTES: usize = 4 * 1024;

pub(crate) struct AutoReviewDispositionHandler;

#[derive(Debug, Deserialize)]
struct AutoReviewDispositionArgs {
    run_id: String,
    action: AutoReviewDispositionAction,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AutoReviewDispositionAction {
    Repair,
    Defer,
    Obsolete,
}

impl ToolExecutor<ToolInvocation> for AutoReviewDispositionHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(AUTO_REVIEW_DISPOSITION_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_auto_review_disposition_tool()
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(self.handle_call(invocation))
    }
}

impl AutoReviewDispositionHandler {
    async fn handle_call(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation { turn, payload, .. } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "{AUTO_REVIEW_DISPOSITION_TOOL_NAME} handler received unsupported payload"
                )));
            }
        };
        let args: AutoReviewDispositionArgs = parse_arguments(&arguments)?;
        let active_review_target = ReviewTarget::UncommittedChanges;
        let selected_cwd = turn.environments.single_local_environment_cwd();
        let cwd = selected_cwd
            .as_ref()
            .map(std::convert::AsRef::as_ref)
            .unwrap_or_else(|| turn.config.cwd.as_ref());
        let active_target =
            collect_auto_review_target(turn.config.codex_home.as_ref(), cwd, &active_review_target)
                .await;
        let store_scope = active_target.worktree_path.as_deref().unwrap_or(cwd);
        let store = AutoReviewStore::for_scope(turn.config.codex_home.as_ref(), store_scope);
        let run = store.load_run(&args.run_id).map_err(respond_to_model)?;
        if args.action != AutoReviewDispositionAction::Obsolete
            && !run.can_read_detail(&active_target, &active_review_target)
        {
            return Err(FunctionCallError::RespondToModel(
                "repair/defer requires a completed Background Review with current findings"
                    .to_string(),
            ));
        }

        let reason = args
            .reason
            .map(|reason| reason.trim().to_string())
            .filter(|reason| !reason.is_empty());
        let disposition = match args.action {
            AutoReviewDispositionAction::Repair => AutoReviewFindingDisposition::Repairing,
            AutoReviewDispositionAction::Defer => AutoReviewFindingDisposition::Deferred,
            AutoReviewDispositionAction::Obsolete => AutoReviewFindingDisposition::Obsolete,
        };
        let is_repair = args.action == AutoReviewDispositionAction::Repair;
        let repair_detail = if is_repair {
            Some(
                store
                    .detail(
                        &args.run_id,
                        /*finding_id*/ None,
                        AUTO_REVIEW_REPAIR_RESPONSE_MAX_BYTES,
                    )
                    .map_err(respond_to_model)?,
            )
        } else {
            None
        };
        let record = AutoReviewFindingDispositionRecord {
            disposition,
            actor: AutoReviewDispositionActor::Agent,
            reason: reason.or_else(|| {
                (args.action == AutoReviewDispositionAction::Repair)
                    .then(|| "bounded repair turn opened by agent".to_string())
            }),
            updated_at_unix_secs: now_unix_timestamp_ms() / 1_000,
        };
        let state = store
            .set_finding_disposition(&args.run_id, record)
            .map_err(respond_to_model)?;
        let record = state.finding_disposition.ok_or_else(|| {
            FunctionCallError::Fatal("auto review disposition was not persisted".to_string())
        })?;

        let mut response = json!({
            "status": "ok",
            "run_id": args.run_id,
            "disposition": record.disposition,
            "actor": record.actor,
            "reason": record.reason,
        });
        if let Some(detail) = repair_detail {
            response["repair_detail"] = json!({
                "bytes": detail.bytes,
                "original_bytes": detail.original_bytes,
                "max_bytes": detail.max_bytes,
                "truncated": detail.truncated,
                "finding_count": detail.finding_count,
                "omitted_findings": detail.omitted_findings,
                "content": detail.content,
            });
        }
        let output = if is_repair {
            serialize_bounded_repair_response(response)?
        } else {
            serialize_response(&response)?
        };
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            output,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for AutoReviewDispositionHandler {}

fn respond_to_model(err: impl std::fmt::Display) -> FunctionCallError {
    FunctionCallError::RespondToModel(err.to_string())
}

fn serialize_bounded_repair_response(mut response: Value) -> Result<String, FunctionCallError> {
    let output = serialize_response(&response)?;
    if output.len() <= AUTO_REVIEW_REPAIR_RESPONSE_MAX_BYTES {
        return Ok(output);
    }
    let content = response
        .pointer("/repair_detail/content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            FunctionCallError::Fatal(
                "auto review repair response is missing detail content".to_string(),
            )
        })?
        .to_string();
    let was_truncated = response
        .pointer("/repair_detail/truncated")
        .and_then(Value::as_bool)
        .unwrap_or_default();
    let mut boundaries = content
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if boundaries.first().copied() != Some(0) {
        boundaries.push(0);
    }
    if boundaries.last().copied() != Some(content.len()) {
        boundaries.push(content.len());
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut lower = 0;
    let mut upper = boundaries.len().saturating_sub(1);
    while lower < upper {
        let middle = (lower + upper).div_ceil(2);
        let end = boundaries[middle];
        set_repair_detail_content(
            &mut response,
            &content[..end],
            was_truncated || end < content.len(),
        )?;
        if serialize_response(&response)?.len() <= AUTO_REVIEW_REPAIR_RESPONSE_MAX_BYTES {
            lower = middle;
        } else {
            upper = middle - 1;
        }
    }

    let end = boundaries[lower];
    set_repair_detail_content(
        &mut response,
        &content[..end],
        was_truncated || end < content.len(),
    )?;
    let output = serialize_response(&response)?;
    if output.len() > AUTO_REVIEW_REPAIR_RESPONSE_MAX_BYTES {
        return Err(FunctionCallError::Fatal(
            "auto review repair response metadata exceeds the output limit".to_string(),
        ));
    }
    Ok(output)
}

fn set_repair_detail_content(
    response: &mut Value,
    content: &str,
    truncated: bool,
) -> Result<(), FunctionCallError> {
    let detail = response
        .get_mut("repair_detail")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            FunctionCallError::Fatal(
                "auto review repair response is missing detail metadata".to_string(),
            )
        })?;
    detail.insert("bytes".to_string(), json!(content.len()));
    detail.insert("truncated".to_string(), json!(truncated));
    detail.insert("content".to_string(), json!(content));
    Ok(())
}

fn serialize_response(response: &Value) -> Result<String, FunctionCallError> {
    serde_json::to_string(response).map_err(|err| {
        FunctionCallError::Fatal(format!(
            "failed to serialize {AUTO_REVIEW_DISPOSITION_TOOL_NAME} response: {err}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_response_is_bounded_after_json_escaping() {
        let response = json!({
            "status": "ok",
            "run_id": "run-1",
            "disposition": "repairing",
            "actor": "agent",
            "reason": "bounded repair turn opened by agent",
            "repair_detail": {
                "bytes": 4096,
                "original_bytes": 4096,
                "max_bytes": 4096,
                "truncated": false,
                "finding_count": 1,
                "omitted_findings": 0,
                "content": "\u{0000}".repeat(4096),
            },
        });

        let output = serialize_bounded_repair_response(response).expect("bounded response");

        assert!(output.len() <= AUTO_REVIEW_REPAIR_RESPONSE_MAX_BYTES);
        let parsed: Value = serde_json::from_str(&output).expect("valid JSON response");
        assert_eq!(
            parsed.pointer("/repair_detail/truncated"),
            Some(&Value::Bool(true))
        );
    }
}
