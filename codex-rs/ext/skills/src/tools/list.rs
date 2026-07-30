use codex_extension_api::FunctionCallError;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolExecutorFuture;
use codex_extension_api::ToolName;
use codex_extension_api::ToolSpec;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::catalog::SkillCatalogEntry;
use crate::render::MAX_SKILL_NAME_BYTES;
use crate::render::truncate_catalog_skill_description;
use crate::render::truncate_utf8_to_bytes;
use crate::warnings::bounded_warnings;

use super::MAX_HANDLE_BYTES;
use super::SkillToolAuthority;
use super::SkillToolAuthoritySelector;
use super::SkillToolContext;
use super::is_bounded_handle;
use super::pagination_cursor;
use super::parse_args;
use super::parse_pagination_cursor;
use super::serialized_fits_output_budget;
use super::serialized_len;
use super::skill_function_tool;
use super::skill_json_output;
use super::skill_tool_name;

const TOOL_NAME: &str = "list";
const MAX_SKILLS_PER_PAGE: usize = 20;
const MAX_LIST_RESPONSE_BYTES: usize = 512 * 1024;
const OVERSIZED_ENTRY_WARNING: &str =
    "Some skills were omitted because their metadata is too large.";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    authority: SkillToolAuthoritySelector,
    cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[schemars(deny_unknown_fields)]
struct ListedSkill {
    authority: SkillToolAuthority,
    package: String,
    name: String,
    description: String,
    main_resource: String,
}

#[derive(Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[schemars(deny_unknown_fields)]
struct ListResponse {
    skills: Vec<ListedSkill>,
    warnings: Vec<String>,
    next_cursor: Option<String>,
}

#[derive(Clone)]
pub(super) struct ListTool {
    pub(super) context: SkillToolContext,
}

impl ToolExecutor<ToolCall> for ListTool {
    fn tool_name(&self) -> ToolName {
        skill_tool_name(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        skill_function_tool::<ListArgs, ListResponse>(
            TOOL_NAME,
            "List skills owned by the requested authority. Returns the exact authority, package, and main_resource values required by skills.read. Pass next_cursor back as cursor to continue.",
        )
    }

    fn handle(&self, call: ToolCall) -> ToolExecutorFuture<'_> {
        Box::pin(async move {
            let args: ListArgs = parse_args(&call)?;
            let catalog = self.context.catalog(&call.turn_id, args.authority).await;
            let mut omitted_oversized_entry = false;
            let skills = catalog
                .entries
                .into_iter()
                .filter(|entry| {
                    entry.is_model_visible() && args.authority.matches(&entry.authority)
                })
                .filter_map(|entry| {
                    let listed = listed_skill(entry).filter(single_entry_response_is_bounded);
                    omitted_oversized_entry |= listed.is_none();
                    listed
                })
                .collect::<Vec<_>>();
            let start = parse_pagination_cursor(args.cursor.as_deref(), &skills, "skills.list")?;
            if start > skills.len() {
                return Err(FunctionCallError::RespondToModel(
                    "skills.list cursor is invalid".to_string(),
                ));
            }
            let warnings = if start == 0 {
                let mut warnings = catalog.warnings;
                if omitted_oversized_entry {
                    warnings.push(OVERSIZED_ENTRY_WARNING.to_string());
                }
                bounded_warnings(&warnings)
            } else {
                Vec::new()
            };
            let response = page_response(&skills, warnings, start, call.truncation_policy)?;
            skill_json_output(&response, args.authority)
        })
    }
}

fn page_response(
    skills: &[ListedSkill],
    mut warnings: Vec<String>,
    start: usize,
    truncation_policy: codex_protocol::protocol::TruncationPolicy,
) -> Result<ListResponse, FunctionCallError> {
    let mut end = (start + MAX_SKILLS_PER_PAGE).min(skills.len());
    loop {
        let response = ListResponse {
            skills: skills[start..end].to_vec(),
            warnings: warnings.clone(),
            next_cursor: (end < skills.len()).then(|| pagination_cursor(skills, end)),
        };
        if serialized_fits_output_budget(&response, truncation_policy, MAX_LIST_RESPONSE_BYTES)? {
            return Ok(response);
        }
        if end.saturating_sub(start) > 1 {
            end -= 1;
        } else if !warnings.is_empty() {
            warnings.pop();
        } else if end == start {
            return Err(FunctionCallError::RespondToModel(
                "skills.list output budget is too small for a paginated response".to_string(),
            ));
        } else {
            let next = start + 1;
            let response = ListResponse {
                skills: Vec::new(),
                warnings: vec![OVERSIZED_ENTRY_WARNING.to_string()],
                next_cursor: (next < skills.len()).then(|| pagination_cursor(skills, next)),
            };
            if serialized_fits_output_budget(&response, truncation_policy, MAX_LIST_RESPONSE_BYTES)?
            {
                return Ok(response);
            }
            return Err(FunctionCallError::RespondToModel(
                "skills.list output budget is too small for a paginated response".to_string(),
            ));
        }
    }
}

fn single_entry_response_is_bounded(skill: &ListedSkill) -> bool {
    serialized_len(&ListResponse {
        skills: vec![skill.clone()],
        warnings: Vec::new(),
        next_cursor: Some(pagination_cursor(skill, usize::MAX)),
    })
    .is_ok_and(|size| size <= MAX_LIST_RESPONSE_BYTES)
}

fn listed_skill(entry: SkillCatalogEntry) -> Option<ListedSkill> {
    let authority = SkillToolAuthority::from_authority(&entry.authority)?;
    if !is_bounded_handle(&entry.authority.id, MAX_HANDLE_BYTES)
        || !is_bounded_handle(&entry.id.0, MAX_HANDLE_BYTES)
        || !is_bounded_handle(entry.main_prompt.as_str(), MAX_HANDLE_BYTES)
    {
        return None;
    }

    Some(ListedSkill {
        authority,
        package: entry.id.0,
        name: truncate_utf8_to_bytes(&entry.name, MAX_SKILL_NAME_BYTES).0,
        description: truncate_catalog_skill_description(&entry.description).into_owned(),
        main_resource: entry.main_prompt.as_str().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use codex_protocol::protocol::TruncationPolicy;
    use codex_utils_string::approx_token_count;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn pages_remain_valid_json_within_model_output_budgets() {
        let skills = (0..40)
            .map(|index| listed_test_skill(index, /*description_bytes*/ 1_024))
            .collect::<Vec<_>>();

        for truncation_policy in [
            TruncationPolicy::Bytes(/*limit*/ 10_000),
            TruncationPolicy::Tokens(/*limit*/ 2_500),
        ] {
            let mut start = 0;
            let mut names = Vec::new();
            let mut page_count = 0;
            loop {
                let response = page_response(&skills, Vec::new(), start, truncation_policy)
                    .expect("skills.list page should fit the model output budget");
                assert_response_fits(&response, truncation_policy);
                assert!(!response.skills.is_empty());
                names.extend(response.skills.iter().map(|skill| skill.name.clone()));
                page_count += 1;

                let Some(cursor) = response.next_cursor else {
                    break;
                };
                start = parse_pagination_cursor(Some(&cursor), &skills, "skills.list")
                    .expect("skills.list cursor should remain valid");
            }

            assert!(page_count > 1);
            assert_eq!(
                names,
                skills
                    .iter()
                    .map(|skill| skill.name.clone())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn oversized_entry_advances_policy_independent_cursor() {
        let skills = vec![
            listed_test_skill(/*index*/ 0, /*description_bytes*/ 4_096),
            listed_test_skill(/*index*/ 1, /*description_bytes*/ 16),
        ];
        let first = page_response(
            &skills,
            Vec::new(),
            /*start*/ 0,
            TruncationPolicy::Bytes(/*limit*/ 512),
        )
        .expect("oversized entry should produce a bounded skip page");
        assert_response_fits(&first, TruncationPolicy::Bytes(/*limit*/ 512));
        assert!(first.skills.is_empty());
        assert_eq!(first.warnings, vec![OVERSIZED_ENTRY_WARNING]);
        let cursor = first
            .next_cursor
            .expect("skip page should advance to the next entry");

        let start = parse_pagination_cursor(Some(&cursor), &skills, "skills.list")
            .expect("cursor should be independent of the output policy");
        let second = page_response(
            &skills,
            Vec::new(),
            start,
            TruncationPolicy::Tokens(/*limit*/ 128),
        )
        .expect("next entry should remain readable under another output policy");
        assert_response_fits(&second, TruncationPolicy::Tokens(/*limit*/ 128));
        assert_eq!(second.skills, vec![skills[1].clone()]);
        assert!(second.next_cursor.is_none());
    }

    #[test]
    fn oversized_entry_is_not_silently_omitted_when_warning_cannot_fit() {
        let skills = vec![listed_test_skill(
            /*index*/ 0, /*description_bytes*/ 4_096,
        )];
        let error = page_response(
            &skills,
            Vec::new(),
            /*start*/ 0,
            TruncationPolicy::Bytes(/*limit*/ 1),
        )
        .expect_err("tiny budget should return a recoverable tool error");

        assert!(matches!(error, FunctionCallError::RespondToModel(_)));
    }

    #[test]
    fn warning_budget_preserves_every_warning_that_fits() {
        let skills = vec![listed_test_skill(
            /*index*/ 0, /*description_bytes*/ 16,
        )];
        let warnings = vec![
            "a".repeat(100),
            "b".repeat(100),
            "c".repeat(100),
            "d".repeat(100),
        ];
        let expected = ListResponse {
            skills: skills.clone(),
            warnings: warnings[..2].to_vec(),
            next_cursor: None,
        };
        let budget = serde_json::to_string(&expected)
            .expect("expected list response should serialize")
            .len();

        let response = page_response(
            &skills,
            warnings,
            /*start*/ 0,
            TruncationPolicy::Bytes(budget),
        )
        .expect("skills.list should retain the warnings that fit");
        assert_eq!(response, expected);
    }

    fn listed_test_skill(index: usize, description_bytes: usize) -> ListedSkill {
        ListedSkill {
            authority: SkillToolAuthority::Executor {
                id: "demo-plugin@1".to_string(),
            },
            package: format!("skill://demo-plugin@1/skills/skill-{index:03}"),
            name: format!("skill-{index:03}"),
            description: "x".repeat(description_bytes),
            main_resource: format!("skill://demo-plugin@1/skills/skill-{index:03}/SKILL.md"),
        }
    }

    fn assert_response_fits(response: &ListResponse, truncation_policy: TruncationPolicy) {
        let serialized = serde_json::to_string(response).expect("list response should serialize");
        match truncation_policy {
            TruncationPolicy::Bytes(limit) => assert!(serialized.len() <= limit),
            TruncationPolicy::Tokens(limit) => {
                assert!(approx_token_count(&serialized) <= limit)
            }
        }
    }
}
