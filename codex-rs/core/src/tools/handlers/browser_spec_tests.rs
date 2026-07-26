use super::*;
use pretty_assertions::assert_eq;

#[test]
fn browser_tool_schema_respects_cdp_policy() {
    let ToolSpec::Function(restricted) = create_browser_tool(/*full_cdp_access*/ false) else {
        panic!("expected function tool");
    };
    let ToolSpec::Function(full) = create_browser_tool(/*full_cdp_access*/ true) else {
        panic!("expected function tool");
    };
    let restricted = serde_json::to_value(restricted.parameters).expect("restricted schema");
    let full = serde_json::to_value(full.parameters).expect("full schema");

    assert_eq!(restricted["required"], json!(["action"]));
    assert!(
        !restricted["properties"]["action"]["enum"]
            .as_array()
            .expect("restricted actions")
            .contains(&json!("cdp"))
    );
    assert!(
        full["properties"]["action"]["enum"]
            .as_array()
            .expect("full actions")
            .contains(&json!("cdp"))
    );
}
