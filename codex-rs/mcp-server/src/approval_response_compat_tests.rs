//! MCP elicitation clients coerce approval replies through
//! `ExecApprovalResponse` / `PatchApprovalResponse`; a deserialization failure
//! silently downgrades the reply to a synthesized denial, so the legacy
//! unit-form `"denied"` decision must keep parsing.

use crate::exec_approval::ExecApprovalResponse;
use crate::patch_approval::PatchApprovalResponse;
use codex_protocol::protocol::ReviewDecision;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn legacy_unit_denied_is_not_coerced_into_a_synthesized_denial() {
    let legacy = json!({"decision": "denied"});

    let exec = serde_json::from_value::<ExecApprovalResponse>(legacy.clone())
        .expect("legacy exec approval response");
    let patch = serde_json::from_value::<PatchApprovalResponse>(legacy)
        .expect("legacy patch approval response");

    assert_eq!(
        [exec.decision, patch.decision],
        [
            ReviewDecision::denied("denied"),
            ReviewDecision::denied("denied")
        ]
    );
}

#[test]
fn current_denied_form_round_trips() {
    let decision = ReviewDecision::denied("not this time");
    let value = serde_json::to_value(ExecApprovalResponse {
        decision: decision.clone(),
    })
    .expect("serialize");

    assert_eq!(
        value,
        json!({"decision": {"denied": {"rejection": "not this time"}}})
    );
    assert_eq!(
        serde_json::from_value::<ExecApprovalResponse>(value)
            .expect("round trip")
            .decision,
        decision
    );
}
