use codex_tools::JsonSchema;
use serde_json::Value;
use serde_json::json;

pub(super) fn bounded_worker_input_schema() -> JsonSchema {
    serde_json::from_value(json!({
        "type": "object",
        "description": "Opt-in bounded external task. Requires explicit external selection and no history fork. Code context must name every task file; incomplete applicable instructions cause refusal. Text context is only for self-contained non-code tasks and receives no code history. One Lab invocation; no automatic retries or substitution. Process limits do not bound provider spending or internal API requests. Provider model/tier/usage remain unobserved; service_tier requests are rejected.",
        "properties": {
            "timeout_ms": {"type": "integer", "description": "End-to-end deadline including preparation and preflight, 1..300000 ms, also capped by the backend timeout."},
            "max_input_bytes": {"type": "integer", "description": "Complete UTF-8 task and instruction payload limit, 1..8192 bytes. Oversized instructions are refused, never truncated."},
            "max_result_bytes": {"type": "integer", "description": "Retained final result or failure-message limit, 256..8192 UTF-8 bytes. Larger results retain a marked tail; this does not cap all subprocess output."},
            "context": {"anyOf": [
                {"type":"object", "properties":{"type":{"type":"string", "enum":["text"]}}, "required":["type"], "additionalProperties":false},
                {"type":"object", "properties":{"type":{"type":"string", "enum":["code"]}, "paths":{"type":"array", "items":{"type":"string"}, "description":"1..32 workspace-relative file paths, each at most 1024 bytes, without parent traversal. Instruction-file edits are refused."}}, "required":["type","paths"], "additionalProperties":false}
            ]}
        },
        "required": ["timeout_ms", "max_input_bytes", "max_result_bytes", "context"],
        "additionalProperties": false
    })).expect("bounded worker input schema")
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
