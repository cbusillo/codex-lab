use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;

pub const BROWSER_TOOL_NAME: &str = "browser";

pub fn create_browser_tool(full_cdp_access: bool) -> ToolSpec {
    let mut actions = vec![
        "open",
        "close",
        "status",
        "click",
        "move",
        "type",
        "key",
        "javascript",
        "scroll",
        "history",
        "inspect",
        "console",
        "cleanup",
        "fetch",
    ];
    if full_cdp_access {
        actions.push("cdp");
    }
    let properties = BTreeMap::from([
        (
            "action".to_string(),
            JsonSchema::string_enum(
                actions.into_iter().map(Value::from).collect(),
                Some(
                    "Required: choose one of the supported browser actions (for example open, click, or fetch)."
                        .to_string(),
                ),
            ),
        ),
        (
            "url".to_string(),
            JsonSchema::string(Some(
                "For action=open or fetch: URL to navigate to or retrieve.".to_string(),
            )),
        ),
        (
            "type".to_string(),
            JsonSchema::string_enum(
                vec![json!("click"), json!("mousedown"), json!("mouseup")],
                Some("For action=click: optional mouse event type.".to_string()),
            ),
        ),
        (
            "x".to_string(),
            JsonSchema::number(Some(
                "For actions=click, move, or inspect: absolute X coordinate.".to_string(),
            )),
        ),
        (
            "y".to_string(),
            JsonSchema::number(Some(
                "For actions=click, move, or inspect: absolute Y coordinate.".to_string(),
            )),
        ),
        (
            "dx".to_string(),
            JsonSchema::number(Some(
                "For action=move or scroll: relative X delta.".to_string(),
            )),
        ),
        (
            "dy".to_string(),
            JsonSchema::number(Some(
                "For action=move or scroll: relative Y delta.".to_string(),
            )),
        ),
        (
            "text".to_string(),
            JsonSchema::string(Some(
                "For action=type: text to send to the focused element.".to_string(),
            )),
        ),
        (
            "key".to_string(),
            JsonSchema::string(Some(
                "For action=key: key to press, such as Enter, Tab, or Escape.".to_string(),
            )),
        ),
        (
            "code".to_string(),
            JsonSchema::string(Some(
                "For action=javascript: JavaScript source to execute.".to_string(),
            )),
        ),
        (
            "direction".to_string(),
            JsonSchema::string_enum(
                vec![json!("back"), json!("forward")],
                Some("For action=history: history direction.".to_string()),
            ),
        ),
        (
            "id".to_string(),
            JsonSchema::string(Some(
                "For action=inspect: optional element id without '#'.".to_string(),
            )),
        ),
        (
            "lines".to_string(),
            JsonSchema::integer(Some(
                "For action=console: optional number of recent console lines.".to_string(),
            )),
        ),
        (
            "method".to_string(),
            JsonSchema::string(Some(
                "For action=cdp: Chrome DevTools Protocol method name.".to_string(),
            )),
        ),
        (
            "params".to_string(),
            JsonSchema::object(BTreeMap::new(), None, Some(true.into())),
        ),
        (
            "target".to_string(),
            JsonSchema::string_enum(
                vec![json!("page"), json!("browser")],
                Some("For action=cdp: target session.".to_string()),
            ),
        ),
        (
            "timeout_ms".to_string(),
            JsonSchema::integer(Some(
                "For action=open or fetch: optional timeout in milliseconds.".to_string(),
            )),
        ),
        (
            "mode".to_string(),
            JsonSchema::string_enum(
                vec![json!("auto"), json!("browser"), json!("http")],
                Some("For action=fetch: optional fetch mode.".to_string()),
            ),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: BROWSER_TOOL_NAME.to_string(),
        description: "Unified browser controller for navigation, interaction, console access, DevTools commands, screenshots, and bounded one-shot fetches."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["action".to_string()]),
            Some(false.into()),
        ),
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "status": { "type": "string" },
                "action": { "type": "string" },
                "error": { "type": ["object", "null"] }
            },
            "required": ["status"],
            "additionalProperties": true
        })),
    })
}

#[cfg(test)]
#[path = "browser_spec_tests.rs"]
mod tests;
