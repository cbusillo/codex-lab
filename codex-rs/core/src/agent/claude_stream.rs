use crate::agent::external_diagnostics::ExternalAgentQuotaDiagnostic;
use serde_json::Value;

const MAX_CONTRACT_VALUE_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeStreamJsonOutput {
    pub(crate) final_message: Option<String>,
    pub(crate) is_error: Option<bool>,
    pub(crate) has_result: bool,
    pub(crate) error_subtype: Option<String>,
    pub(crate) quota_diagnostic: Option<ExternalAgentQuotaDiagnostic>,
}

/// Decode the bounded subset of Claude Code's `stream-json` output that an
/// external agent needs. Unknown event types and fields are intentionally
/// ignored so newer CLI event fields do not become a compatibility break.
pub(crate) fn parse_claude_stream_json(output: &[u8]) -> Option<ClaudeStreamJsonOutput> {
    let output = String::from_utf8_lossy(output);
    let mut assistant_message = String::new();
    let mut result_message = None;
    let mut is_error = None;
    let mut has_result = false;
    let mut error_subtype = None;
    let mut quota_diagnostic = None;
    let mut recognized_event = false;

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(object) = event.as_object() else {
            continue;
        };
        let Some(event_type) = object.get("type").and_then(Value::as_str) else {
            continue;
        };
        match event_type {
            "assistant" => {
                let Some(content) = event.pointer("/message/content").and_then(Value::as_array)
                else {
                    continue;
                };
                for item in content {
                    if item.get("type").and_then(Value::as_str) == Some("text")
                        && let Some(text) = item.get("text").and_then(Value::as_str)
                    {
                        if !assistant_message.is_empty() {
                            assistant_message.push('\n');
                        }
                        assistant_message.push_str(text);
                        recognized_event = true;
                    }
                }
            }
            "result" => {
                recognized_event = true;
                has_result = true;
                if let Some(value) = object.get("is_error") {
                    is_error = value.as_bool();
                }
                if let Some(value) = object.get("result") {
                    result_message = value.as_str().map(str::to_string);
                }
                error_subtype = object
                    .get("subtype")
                    .and_then(Value::as_str)
                    .and_then(bounded_contract_value);
            }
            "rate_limit_event" => {
                if let Some(diagnostic) = object
                    .get("rate_limit_info")
                    .and_then(|value| parse_rate_limit_info(value).ok())
                {
                    recognized_event = true;
                    quota_diagnostic = Some(diagnostic);
                }
            }
            _ => {}
        }
    }

    if !recognized_event {
        return None;
    }

    let final_message = result_message
        .filter(|message| !message.trim().is_empty())
        .or_else(|| (!assistant_message.trim().is_empty()).then_some(assistant_message));
    Some(ClaudeStreamJsonOutput {
        final_message,
        is_error,
        has_result,
        error_subtype,
        quota_diagnostic,
    })
}

fn parse_rate_limit_info(value: &Value) -> Result<ExternalAgentQuotaDiagnostic, ()> {
    let object = value.as_object().ok_or(())?;
    Ok(ExternalAgentQuotaDiagnostic {
        status: contract_string(object, "status")?,
        window: contract_string(object, "rateLimitType")?,
        resets_at: optional_timestamp(object, "resetsAt")?,
        overage_state: optional_contract_string(object, "overageStatus")?
            .unwrap_or_else(|| "unknown".to_string()),
        overage_reason: optional_contract_string(object, "overageDisabledReason")?,
        is_using_overage: object
            .get("isUsingOverage")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn contract_string(object: &serde_json::Map<String, Value>, field: &str) -> Result<String, ()> {
    object
        .get(field)
        .and_then(Value::as_str)
        .and_then(bounded_contract_value)
        .ok_or(())
}

fn optional_contract_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<String>, ()> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(value.as_str().and_then(bounded_contract_value)),
    }
}

fn optional_timestamp(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<i64>, ()> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(value.as_i64()),
    }
}

fn bounded_contract_value(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() <= MAX_CONTRACT_VALUE_BYTES && !value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
#[path = "claude_stream_tests.rs"]
mod tests;
