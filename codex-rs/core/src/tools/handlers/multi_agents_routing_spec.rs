use codex_tools::JsonSchema;
use serde_json::Value;
use serde_json::json;

const VISIBLE_PROVIDER_ROUTING_GUIDANCE: &str = "Classify every delegated task with `task_kind` and `task_size`. If the user names an agent, provider, or model, you MUST set `agent_type` to its canonical selector; explicit user selection overrides automatic routing and must never be encoded only in `task_name`. For normal or large independent review, security, infrastructure, release, and product-risk work where the user did not name an agent, omit `agent_type` so Codex Lab can select an eligible installed third-party agent. Tiny tasks stay native by default. `agent_type = \"default\"` explicitly accepts a native agent.";
const HIDDEN_PROVIDER_ROUTING_GUIDANCE: &str = "Classify every delegated task with `task_kind` and `task_size`. For normal or large independent review, security, infrastructure, release, and product-risk work, Codex Lab selects an eligible installed third-party agent when available. Tiny tasks stay native by default. Agent-type overrides are not exposed in this configuration.";
const AGENT_TASK_KIND_DESCRIPTION: &str = "Task category used by Codex Lab's provider-routing policy. Use `independent_review` for a dissenting review and the matching risk category for security, infrastructure, release, or product-risk work.";
const AGENT_TASK_SIZE_DESCRIPTION: &str = "Task significance used by provider routing. Use `tiny` only for bounded low-risk work; normal and large high-risk work may route to an eligible third-party agent.";

pub(super) fn provider_routing_guidance(agent_type_visible: bool) -> &'static str {
    if agent_type_visible {
        VISIBLE_PROVIDER_ROUTING_GUIDANCE
    } else {
        HIDDEN_PROVIDER_ROUTING_GUIDANCE
    }
}

pub(super) fn create_agent_task_kind_schema() -> JsonSchema {
    JsonSchema::string_enum(
        vec![
            json!("implementation"),
            json!("research"),
            json!("independent_review"),
            json!("security"),
            json!("infrastructure"),
            json!("release"),
            json!("product_risk"),
            json!("other"),
        ],
        Some(AGENT_TASK_KIND_DESCRIPTION.to_string()),
    )
}

pub(super) fn create_agent_task_size_schema() -> JsonSchema {
    JsonSchema::string_enum(
        vec![json!("tiny"), json!("normal"), json!("large")],
        Some(AGENT_TASK_SIZE_DESCRIPTION.to_string()),
    )
}

pub(super) fn provider_routing_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["explicit", "automatic_external", "native_default", "native_fallback"]
            },
            "requested": { "type": "string" },
            "effective": { "type": "string" },
            "reason": { "type": "string" },
            "skipped_candidates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "selector": { "type": "string" },
                        "kind": {
                            "type": "string",
                            "enum": ["disabled", "unavailable"]
                        },
                        "failure_kind": {
                            "type": "string",
                            "enum": [
                                "command_missing",
                                "authentication_required",
                                "quota_or_rate_limited",
                                "unsupported_mode",
                                "timed_out",
                                "malformed_output",
                                "empty_output",
                                "launch_failed",
                                "provider_failed"
                            ]
                        },
                        "reason": { "type": "string" }
                    },
                    "required": ["selector", "kind", "reason"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["kind", "effective", "reason"],
        "additionalProperties": false
    })
}

pub(super) fn external_agent_provider_output_schema() -> Value {
    json!({
        "type": "object",
        "description": "External-provider provenance when spawn metadata is visible.",
        "properties": {
            "agent_type": { "type": "string" },
            "provider_family": { "type": "string" },
            "command": { "type": "string" },
            "cli_version": { "type": "string" },
            "capability_source": {
                "type": "string",
                "enum": ["static_catalog", "local_cli", "not_probed", "conservative_fallback"]
            },
            "capability_freshness": {
                "type": "string",
                "enum": ["fresh", "cached"]
            },
            "protocol": {
                "type": "string",
                "enum": ["json", "raw_cli"]
            },
            "mode": {
                "type": "string",
                "enum": ["read_only", "write"]
            },
            "workspace": { "type": "string" },
            "model": { "type": "string" },
            "effort": { "type": "string" }
        },
        "required": [
            "command",
            "capability_source",
            "protocol",
            "mode",
            "workspace"
        ],
        "additionalProperties": false
    })
}

pub(super) fn external_agent_failure_output_schema() -> Value {
    json!({
        "type": "object",
        "description": "Structured external-provider failure details when the agent failed.",
        "properties": {
            "kind": {
                "type": "string",
                "enum": [
                    "command_missing",
                    "authentication_required",
                    "quota_or_rate_limited",
                    "unsupported_mode",
                    "timed_out",
                    "malformed_output",
                    "empty_output",
                    "launch_failed",
                    "provider_failed"
                ]
            },
            "message": { "type": "string" }
        },
        "required": ["kind"],
        "additionalProperties": false
    })
}
