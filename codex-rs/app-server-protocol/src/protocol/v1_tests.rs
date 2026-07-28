use super::v1::ApplyPatchApprovalResponse;
use super::v1::ExecCommandApprovalResponse;
use codex_protocol::protocol::ReviewDecision;
use pretty_assertions::assert_eq;
use serde_json::json;

/// v1 clients built before `Denied` carried a rejection string send the bare
/// `"denied"` variant name.
#[test]
fn approval_responses_accept_the_legacy_unit_denied_decision() {
    let legacy = json!({"decision": "denied"});

    assert_eq!(
        serde_json::from_value::<ExecCommandApprovalResponse>(legacy.clone())
            .expect("legacy exec approval response"),
        ExecCommandApprovalResponse {
            decision: ReviewDecision::denied("denied"),
        }
    );
    assert_eq!(
        serde_json::from_value::<ApplyPatchApprovalResponse>(legacy)
            .expect("legacy patch approval response"),
        ApplyPatchApprovalResponse {
            decision: ReviewDecision::denied("denied"),
        }
    );
}

#[test]
fn approval_responses_round_trip_the_current_denied_decision() {
    let exec = ExecCommandApprovalResponse {
        decision: ReviewDecision::denied("not this time"),
    };
    let patch = ApplyPatchApprovalResponse {
        decision: ReviewDecision::denied("not this time"),
    };

    let exec_value = serde_json::to_value(&exec).expect("serialize exec response");
    let patch_value = serde_json::to_value(&patch).expect("serialize patch response");

    assert_eq!(
        [exec_value.clone(), patch_value.clone()],
        [
            json!({"decision": {"denied": {"rejection": "not this time"}}}),
            json!({"decision": {"denied": {"rejection": "not this time"}}}),
        ]
    );
    assert_eq!(
        serde_json::from_value::<ExecCommandApprovalResponse>(exec_value).expect("exec round trip"),
        exec
    );
    assert_eq!(
        serde_json::from_value::<ApplyPatchApprovalResponse>(patch_value)
            .expect("patch round trip"),
        patch
    );
}
