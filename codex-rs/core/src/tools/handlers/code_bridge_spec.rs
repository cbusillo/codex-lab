use codex_code_bridge_protocol::MAX_CONTROL_TIMEOUT_MS;
use codex_code_bridge_protocol::MAX_EVENT_TEXT_BYTES;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;

pub const CODE_BRIDGE_TOOL_NAME: &str = "code_bridge";

pub fn create_code_bridge_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::string_enum(
                vec![json!("status"), json!("subscribe"), json!("screenshot"), json!("javascript")],
                Some(
                    "Required: status (read bridge service state), subscribe (reserved for event streaming), screenshot (request a screenshot), javascript (run JS on a bridge client)."
                        .to_string(),
                ),
            ),
        ),
        (
            "targetClientId".to_string(),
            JsonSchema::string(Some(
                "For action=screenshot or action=javascript: Code Bridge producer client id to target."
                    .to_string(),
            )),
        ),
        (
            "code".to_string(),
            JsonSchema::string(Some(format!(
                "For action=javascript: JavaScript source to execute on the bridge client. Maximum {MAX_EVENT_TEXT_BYTES} bytes."
            ))),
        ),
        (
            "timeoutMs".to_string(),
            JsonSchema::number(Some(format!(
                "Optional control timeout in milliseconds. Defaults to {MAX_CONTROL_TIMEOUT_MS}; maximum {MAX_CONTROL_TIMEOUT_MS}."
            ))),
        ),
        (
            "level".to_string(),
            JsonSchema::string_enum(
                vec![json!("errors"), json!("warn"), json!("info"), json!("trace")],
                Some("For action=subscribe: desired event level. Event streaming is reserved until the core bridge listener is enabled.".to_string()),
            ),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: CODE_BRIDGE_TOOL_NAME.to_string(),
        description: "Code Bridge = local app/browser telemetry and control bridge. Use status to inspect availability, screenshot to request a capture from a target bridge client, and javascript to execute JS on a target bridge client."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["action".to_string()]),
            Some(false.into()),
        ),
        output_schema: Some(code_bridge_output_schema()),
    })
}

fn code_bridge_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": { "type": "string" },
            "message": { "type": "string" },
            "service": { "type": "object" },
            "requestId": { "type": "string" },
            "screenshot": { "type": ["object", "null"] },
            "summary": { "type": "string" },
            "result": {},
            "error": { "type": ["object", "null"] }
        },
        "required": ["status"],
        "additionalProperties": true
    })
}

#[cfg(test)]
#[path = "code_bridge_spec_tests.rs"]
mod tests;
