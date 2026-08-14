use super::*;
use pretty_assertions::assert_eq;

const ASSISTANT_SUCCESS: &[u8] = include_bytes!("fixtures/claude_stream/assistant_success.jsonl");
const FIVE_HOUR_REJECTED: &[u8] = include_bytes!("fixtures/claude_stream/five_hour_rejected.jsonl");
const WEEKLY_REJECTED: &[u8] = include_bytes!("fixtures/claude_stream/weekly_rejected.jsonl");
const OVERAGE_REJECTED: &[u8] = include_bytes!("fixtures/claude_stream/overage_rejected.jsonl");
const MALFORMED: &[u8] = include_bytes!("fixtures/claude_stream/malformed.jsonl");

#[test]
fn decodes_allowed_stream_without_transport_events() {
    let output =
        parse_claude_stream_json(ASSISTANT_SUCCESS).expect("fixture should contain Claude events");

    assert_eq!(
        output.final_message.as_deref(),
        Some("Repository inspection complete.")
    );
    assert_eq!(output.is_error, Some(false));
    assert!(output.has_result);
    assert_eq!(
        output.quota_diagnostic,
        Some(ExternalAgentQuotaDiagnostic {
            status: "allowed".to_string(),
            window: "five_hour".to_string(),
            resets_at: Some(1_783_830_000),
            overage_state: "rejected".to_string(),
            overage_reason: Some("org_level_disabled_until".to_string()),
            is_using_overage: false,
        })
    );
}

#[test]
fn decodes_rejected_quota_windows_and_overage_state() {
    for (fixture, window, resets_at, overage_reason) in [
        (
            FIVE_HOUR_REJECTED,
            "five_hour",
            1_783_830_000,
            Some("org_level_disabled_until"),
        ),
        (WEEKLY_REJECTED, "weekly", 1_784_434_800, None),
        (
            OVERAGE_REJECTED,
            "five_hour",
            1_783_830_000,
            Some("organization_policy"),
        ),
    ] {
        let output =
            parse_claude_stream_json(fixture).expect("fixture should contain Claude events");
        assert_eq!(output.is_error, Some(true));
        assert_eq!(
            output.quota_diagnostic,
            Some(ExternalAgentQuotaDiagnostic {
                status: "rejected".to_string(),
                window: window.to_string(),
                resets_at: Some(resets_at),
                overage_state: if window == "weekly" {
                    "not_available".to_string()
                } else {
                    "rejected".to_string()
                },
                overage_reason: overage_reason.map(str::to_string),
                is_using_overage: false,
            })
        );
    }
}

#[test]
fn ignores_malformed_stream_without_claiming_stream_support() {
    assert_eq!(parse_claude_stream_json(MALFORMED), None);
}

#[test]
fn ignores_unknown_events_without_claiming_stream_support() {
    assert_eq!(
        parse_claude_stream_json(b"{\"type\":\"future_event\",\"private\":\"payload\"}\n"),
        None
    );
}

#[test]
fn keeps_incomplete_streams_on_the_raw_compatibility_path() {
    let output = parse_claude_stream_json(
        br#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour","overageStatus":"available","isUsingOverage":false}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Partial response"}]}}
"#,
    )
    .expect("partial stream contains Claude events");

    assert_eq!(output.final_message.as_deref(), Some("Partial response"));
    assert!(!output.has_result);
}

#[test]
fn preserves_valid_events_around_corrupt_lines() {
    let output = parse_claude_stream_json(
        br#"{"type":"assistant","message":{"content":[{"type":"text","text":"Recovered assistant"}]}}
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}
not json
{"type":"result","is_error":false,"result":"Recovered result"}
"#,
    )
    .expect("valid events should survive corrupt lines");

    assert_eq!(output.final_message.as_deref(), Some("Recovered result"));
    assert_eq!(output.is_error, Some(false));
    assert!(output.has_result);
    assert_eq!(output.quota_diagnostic, None);
}

#[test]
fn recovers_result_from_marker_prefixed_truncated_tail() {
    let output = parse_claude_stream_json(
        br#"[external agent stdout truncated]
partial leading bytes
{"type":"result","is_error":false,"result":"Recovered tail result"}
"#,
    )
    .expect("valid tail result should survive truncation");

    assert_eq!(
        output.final_message.as_deref(),
        Some("Recovered tail result")
    );
    assert_eq!(output.is_error, Some(false));
    assert!(output.has_result);
}

#[test]
fn keeps_primary_quota_fields_when_overage_fields_are_missing() {
    let output = parse_claude_stream_json(
        br#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour","resetsAt":1783830000}}
"#,
    )
    .expect("primary quota fields should remain authoritative");

    assert_eq!(
        output.quota_diagnostic,
        Some(ExternalAgentQuotaDiagnostic {
            status: "rejected".to_string(),
            window: "five_hour".to_string(),
            resets_at: Some(1_783_830_000),
            overage_state: "unknown".to_string(),
            overage_reason: None,
            is_using_overage: false,
        })
    );
}
