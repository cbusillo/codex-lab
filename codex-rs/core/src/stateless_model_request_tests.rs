use super::StatelessModelRequest;
use super::build_prompt;
use super::parse_structured_output;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use serde_json::json;

#[test]
fn prompt_contains_only_explicit_instructions_input_and_schema() {
    let schema = json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    });
    let prompt = build_prompt(StatelessModelRequest {
        model: "gpt-test".to_string(),
        developer_instructions: "Return the requested JSON.".to_string(),
        user_input: "Summarize this.".to_string(),
        output_schema: schema.clone(),
        max_output_tokens: Some(123),
    });

    assert_eq!(prompt.base_instructions.text, "Return the requested JSON.");
    assert_eq!(prompt.output_schema, Some(schema));
    assert!(prompt.output_schema_strict);
    assert_eq!(prompt.max_output_tokens, Some(123));
    assert!(prompt.tools.is_empty());
    assert!(!prompt.parallel_tool_calls);
    assert_eq!(prompt.input.len(), 1);
    assert!(matches!(
        &prompt.input[0],
        ResponseItem::Message { role, content, .. }
            if role == "user"
                && content == &vec![ContentItem::InputText {
                    text: "Summarize this.".to_string()
                }]
    ));
}

#[test]
fn structured_output_requires_nonempty_valid_json() {
    assert_eq!(
        parse_structured_output(r#"{"answer":"ok"}"#).expect("valid JSON"),
        json!({"answer": "ok"})
    );
    assert!(parse_structured_output("   ").is_err());
    assert!(parse_structured_output("not json").is_err());
}
