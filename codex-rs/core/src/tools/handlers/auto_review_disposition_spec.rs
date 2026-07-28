use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) const AUTO_REVIEW_DISPOSITION_TOOL_NAME: &str = "auto_review_disposition";

pub(crate) fn create_auto_review_disposition_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "run_id".to_string(),
            JsonSchema::string(Some(
                "Stable Background Review run id from auto-review awareness.".to_string(),
            )),
        ),
        (
            "action".to_string(),
            JsonSchema::string_enum(
                vec![json!("repair"), json!("defer"), json!("obsolete")],
                Some(
                    "repair opens a bounded repair context, defer acknowledges the findings for later, and obsolete dismisses them with a required reason."
                        .to_string(),
                ),
            ),
        ),
        (
            "reason".to_string(),
            JsonSchema::string(Some(
                "Short audited reason. Required when action is obsolete.".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: AUTO_REVIEW_DISPOSITION_TOOL_NAME.to_string(),
        description: "Disposition current Background Review findings. Use repair before applying a bounded fix, defer to acknowledge them for later, or obsolete with a reason when they no longer apply. Repair returns only bounded review detail."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["run_id".to_string(), "action".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}
