use codex_extension_api::FunctionCallError;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolExecutorFuture;
use codex_extension_api::ToolName;
use codex_extension_api::ToolSpec;
use codex_protocol::protocol::TruncationPolicy;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::catalog::SkillResourceId;
use crate::provider::SkillReadRequest;

use super::MAX_HANDLE_BYTES;
use super::SkillToolAuthority;
use super::SkillToolContext;
use super::pagination_cursor;
use super::parse_args;
use super::parse_pagination_cursor;
use super::serialized_fits_output_budget;
use super::skill_function_tool;
use super::skill_json_output;
use super::skill_tool_name;
use super::validate_handle;

const TOOL_NAME: &str = "read";
const MAX_READ_RESPONSE_BYTES: usize = 512 * 1024;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    authority: SkillToolAuthority,
    package: String,
    resource: String,
    cursor: Option<String>,
}

#[derive(Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[schemars(deny_unknown_fields)]
struct ReadResponse {
    resource: String,
    contents: String,
    next_cursor: Option<String>,
}

#[derive(Clone)]
pub(super) struct ReadTool {
    pub(super) context: SkillToolContext,
}

impl ToolExecutor<ToolCall> for ReadTool {
    fn tool_name(&self) -> ToolName {
        skill_tool_name(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        skill_function_tool::<ReadArgs, ReadResponse>(
            TOOL_NAME,
            "Read one page from a skill resource. Pass the exact authority and package from skills.list or an explicitly selected skill's resource_access metadata, plus its main_resource or a referenced resource beneath that package. Pass next_cursor back as cursor to continue.",
        )
    }

    fn handle(&self, call: ToolCall) -> ToolExecutorFuture<'_> {
        Box::pin(async move {
            let args: ReadArgs = parse_args(&call)?;
            if let SkillToolAuthority::Executor { id } = &args.authority {
                validate_handle("authority.id", id, MAX_HANDLE_BYTES)?;
            }
            validate_handle("package", &args.package, MAX_HANDLE_BYTES)?;
            validate_handle("resource", &args.resource, MAX_HANDLE_BYTES)?;

            let output_authority = args.authority.selector();
            let catalog = self.context.catalog(&call.turn_id, output_authority).await;
            let Some(skill_entry) = catalog.entries.iter().find(|entry| {
                entry.enabled
                    && args.authority.matches(&entry.authority)
                    && entry.id.0 == args.package
            }) else {
                return Err(FunctionCallError::RespondToModel(
                    "skill package is not available from the requested authority".to_string(),
                ));
            };
            let authority = skill_entry.authority.clone();
            let package = skill_entry.id.clone();
            let main_prompt = skill_entry.main_prompt.clone();
            let requested_resource = if args.resource == main_prompt.as_str() {
                main_prompt.clone()
            } else {
                main_prompt
                    .bind_environment_package_resource(&package, args.resource.clone())
                    .unwrap_or_else(|| SkillResourceId::new(args.resource))
            };
            let resolved_executor_roots = self
                .context
                .executor_query
                .as_ref()
                .map(|query| query.resolved_executor_roots.clone())
                .unwrap_or_default();
            let sandbox = requested_resource
                .environment_path()
                .and_then(|(environment_id, _)| {
                    self.context.sandbox_contexts.as_ref().and_then(|contexts| {
                        contexts.get(environment_id).map(|captured| {
                            call.environments
                                .iter()
                                .find(|environment| environment.environment_id == environment_id)
                                .map(|environment| environment.file_system_sandbox_context.clone())
                                .unwrap_or_else(|| captured.clone())
                        })
                    })
                });
            if self.context.sandbox_contexts.is_some()
                && requested_resource.environment_path().is_some()
                && sandbox.is_none()
            {
                return Err(FunctionCallError::RespondToModel(
                    "failed to read skill resource".to_string(),
                ));
            }
            let result = self
                .context
                .thread_state
                .read_skill(
                    &self.context.providers,
                    SkillReadRequest {
                        authority,
                        package,
                        resource: requested_resource.clone(),
                        resolved_executor_roots,
                        sandbox,
                        host_snapshot: None,
                        mcp_resources: self.context.mcp_resources.clone(),
                    },
                )
                .await
                .map_err(|err| {
                    tracing::warn!(
                        error = %err,
                        turn_id = %call.turn_id,
                        call_id = %call.call_id,
                        resource = requested_resource.as_str(),
                        "skills.read provider request failed"
                    );
                    FunctionCallError::RespondToModel("failed to read skill resource".to_string())
                })?;
            if result.resource != requested_resource {
                return Err(FunctionCallError::Fatal(
                    "skill provider returned a different resource".to_string(),
                ));
            }
            if output_authority == super::SkillToolAuthoritySelector::Orchestrator
                && let Some(state) = self
                    .context
                    .thread_state
                    .shadow_selection_turn(&call.turn_id)
            {
                self.context
                    .shadow_selection
                    .record_invocation(&state, main_prompt.as_str());
            }

            let start = parse_pagination_cursor(
                args.cursor.as_deref(),
                result.contents.as_str(),
                "skills.read",
            )?;
            if start > result.contents.len() || !result.contents.is_char_boundary(start) {
                return Err(FunctionCallError::RespondToModel(
                    "skills.read cursor is invalid".to_string(),
                ));
            }
            let response = page_response(
                result.resource.as_str(),
                &result.contents,
                start,
                call.truncation_policy,
            )?;
            skill_json_output(&response, output_authority)
        })
    }
}

fn page_response(
    resource: &str,
    contents: &str,
    start: usize,
    truncation_policy: TruncationPolicy,
) -> Result<ReadResponse, FunctionCallError> {
    let response = |end, next_cursor| ReadResponse {
        resource: resource.to_string(),
        contents: contents[start..end].to_string(),
        next_cursor,
    };
    let complete = response(contents.len(), None);
    if serialized_fits_output_budget(&complete, truncation_policy, MAX_READ_RESPONSE_BYTES)? {
        return Ok(complete);
    }

    let mut low = start;
    let mut high = contents.len();
    let mut best = None;
    while high.saturating_sub(low) > 1 {
        let midpoint = low + (high - low) / 2;
        let mut end = midpoint;
        while end > low && !contents.is_char_boundary(end) {
            end -= 1;
        }
        if end == low {
            end = midpoint;
            while end < high && !contents.is_char_boundary(end) {
                end += 1;
            }
            if end == high {
                break;
            }
        }
        let candidate = response(end, Some(pagination_cursor(contents, end)));
        if serialized_fits_output_budget(&candidate, truncation_policy, MAX_READ_RESPONSE_BYTES)? {
            low = end;
            best = Some(candidate);
        } else {
            high = end;
        }
    }
    best.ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "skill resource handle leaves no room for contents".to_string(),
        )
    })
}

#[cfg(test)]
mod tests {
    use codex_utils_string::approx_token_count;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn pages_reconstruct_resource_within_model_output_budgets() {
        let contents = "abcd💡".repeat(8_000);

        for truncation_policy in [
            TruncationPolicy::Bytes(/*limit*/ 10_000),
            TruncationPolicy::Tokens(/*limit*/ 2_500),
        ] {
            let mut start = 0;
            let mut reconstructed = String::new();
            let mut page_count = 0;
            loop {
                let response = page_response(
                    "skill://demo-plugin@1/skills/deploy/references/details.md",
                    &contents,
                    start,
                    truncation_policy,
                )
                .expect("skills.read page should fit the model output budget");
                let serialized =
                    serde_json::to_string(&response).expect("read response should serialize");
                match truncation_policy {
                    TruncationPolicy::Bytes(limit) => assert!(serialized.len() <= limit),
                    TruncationPolicy::Tokens(limit) => {
                        assert!(approx_token_count(&serialized) <= limit)
                    }
                }
                assert!(!response.contents.is_empty());
                reconstructed.push_str(&response.contents);
                page_count += 1;

                let Some(cursor) = response.next_cursor else {
                    break;
                };
                start = parse_pagination_cursor(Some(&cursor), &contents, "skills.read")
                    .expect("skills.read cursor should remain valid");
            }

            assert!(page_count > 1);
            assert_eq!(reconstructed, contents);
        }
    }

    #[test]
    fn tiny_budget_does_not_emit_nonadvancing_page() {
        let error = page_response(
            "skill://demo-plugin@1/skills/deploy/SKILL.md",
            "x",
            /*start*/ 0,
            TruncationPolicy::Bytes(/*limit*/ 1),
        )
        .expect_err("tiny budget should return a recoverable tool error");
        assert!(matches!(error, FunctionCallError::RespondToModel(_)));
    }

    #[test]
    fn page_search_tries_the_next_unicode_boundary() {
        let resource = "skill://demo-plugin@1/skills/deploy/SKILL.md";
        let contents = format!("{}💡{}", "a".repeat(100), "b".repeat(100));
        let expected_contents = format!("{}💡", "a".repeat(100));
        let expected = ReadResponse {
            resource: resource.to_string(),
            contents: expected_contents.clone(),
            next_cursor: Some(pagination_cursor(&contents, expected_contents.len())),
        };
        let budget = serde_json::to_string(&expected)
            .expect("expected read response should serialize")
            .len();

        assert_eq!(
            page_response(
                resource,
                &contents,
                /*start*/ 0,
                TruncationPolicy::Bytes(budget),
            )
            .expect("one complete Unicode scalar should fit"),
            expected
        );
    }
}
