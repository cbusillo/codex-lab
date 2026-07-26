use super::CODE_BRIDGE_TOOL_NAME;
use super::create_code_bridge_tool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn code_bridge_tool_schema_exposes_expected_actions() {
    let ToolSpec::Function(tool) = create_code_bridge_tool() else {
        panic!("expected function tool");
    };

    assert_eq!(tool.name, CODE_BRIDGE_TOOL_NAME);
    assert!(tool.description.contains("Code Bridge"));
    let schema = serde_json::to_value(&tool.parameters).expect("serialize schema");
    assert_eq!(
        schema["properties"]["action"]["enum"],
        json!(["status", "subscribe", "screenshot", "javascript"])
    );
    assert_eq!(schema["required"], json!(["action"]));
}
