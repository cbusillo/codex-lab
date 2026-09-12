use codex_tools::JsonSchema;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;

pub(super) fn bounded_worker_input_schema() -> JsonSchema {
    let text_context = JsonSchema::object(
        BTreeMap::from([(
            "type".to_string(),
            JsonSchema::string_enum(vec![json!("text")], /*description*/ None),
        )]),
        Some(vec!["type".to_string()]),
        Some(false.into()),
    );
    let code_context = JsonSchema::object(
        BTreeMap::from([
            (
                "type".to_string(),
                JsonSchema::string_enum(vec![json!("code")], /*description*/ None),
            ),
            (
                "paths".to_string(),
                JsonSchema::array(
                    JsonSchema::string(/*description*/ None),
                    Some("1..32 workspace-relative file paths, each at most 1024 bytes, without parent traversal. Instruction-file edits are refused.".to_string()),
                ),
            ),
        ]),
        Some(vec!["type".to_string(), "paths".to_string()]),
        Some(false.into()),
    );
    JsonSchema {
        description: Some("Opt-in bounded external task. Requires explicit external selection and no history fork. Code context must name every task file; incomplete applicable instructions cause refusal. Text context is only for self-contained non-code tasks and receives no code history. One Lab invocation; no automatic retries or substitution. Process limits do not bound provider spending or internal API requests. Provider model/tier/usage remain unobserved; service_tier requests are rejected.".to_string()),
        ..JsonSchema::object(
            BTreeMap::from([
                (
                    "timeout_ms".to_string(),
                    JsonSchema::integer(Some("End-to-end deadline including preparation and preflight, 1..300000 ms, also capped by the backend timeout.".to_string())),
                ),
                (
                    "max_input_bytes".to_string(),
                    JsonSchema::integer(Some("Complete UTF-8 task, instructions and serialized routing-envelope limit, 1..8192 bytes. Oversized instructions are refused, never truncated.".to_string())),
                ),
                (
                    "max_result_bytes".to_string(),
                    JsonSchema::integer(Some("Retained final result or failure-message limit, 256..8192 UTF-8 bytes. Larger results retain a marked tail; this does not cap all subprocess output.".to_string())),
                ),
                (
                    "context".to_string(),
                    JsonSchema::any_of(vec![text_context, code_context], /*description*/ None),
                ),
            ]),
            Some(vec!["timeout_ms".to_string(), "max_input_bytes".to_string(), "max_result_bytes".to_string(), "context".to_string()]),
            Some(false.into()),
        )
    }
}

pub(super) fn bounded_worker_output_schema() -> Value {
    json!({
        "type": "object",
        "description": "Accepted process/payload limits. Configuration identifies the requested provider; actual provider model, tier, internal request count and spending are not observed.",
        "properties": {
            "timeout_ms": {"type":"integer"},
            "max_input_bytes": {"type":"integer"},
            "max_result_bytes": {"type":"integer"}
        },
        "required": ["timeout_ms", "max_input_bytes", "max_result_bytes"],
        "additionalProperties": false
    })
}
