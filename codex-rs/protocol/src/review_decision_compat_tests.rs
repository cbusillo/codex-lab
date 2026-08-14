use crate::approvals::NetworkPolicyRuleAction;
use crate::protocol::ExecPolicyAmendment;
use crate::protocol::NetworkPolicyAmendment;
use crate::protocol::ReviewDecision;
use pretty_assertions::assert_eq;
use serde_json::json;

fn all_variants() -> Vec<ReviewDecision> {
    vec![
        ReviewDecision::Approved,
        ReviewDecision::ApprovedExecpolicyAmendment {
            proposed_execpolicy_amendment: ExecPolicyAmendment::new(vec![
                "git".to_string(),
                "status".to_string(),
            ]),
        },
        ReviewDecision::ApprovedForSession,
        ReviewDecision::ApprovedMcpPolicyAmendment,
        ReviewDecision::NetworkPolicyAmendment {
            network_policy_amendment: NetworkPolicyAmendment {
                host: "example.com".to_string(),
                action: NetworkPolicyRuleAction::Allow,
            },
        },
        ReviewDecision::denied("nope"),
        ReviewDecision::TimedOut,
        ReviewDecision::Abort,
    ]
}

#[test]
fn every_variant_round_trips_through_the_current_wire_form() {
    let variants = all_variants();

    let round_tripped = variants
        .iter()
        .map(|decision| {
            let value = serde_json::to_value(decision).expect("serialize");
            serde_json::from_value::<ReviewDecision>(value).expect("deserialize")
        })
        .collect::<Vec<_>>();

    assert_eq!(round_tripped, variants);
}

/// Clients built against the pre-`rejection` protocol send `Denied` as the bare
/// string `"denied"`.
#[test]
fn legacy_unit_denied_deserializes_to_the_default_rejection() {
    assert_eq!(
        serde_json::from_value::<ReviewDecision>(json!("denied")).expect("legacy denied"),
        ReviewDecision::denied("denied"),
    );
}

#[test]
fn legacy_unit_denied_reserializes_in_the_current_form() {
    let decision =
        serde_json::from_value::<ReviewDecision>(json!("denied")).expect("legacy denied");

    assert_eq!(
        serde_json::to_value(&decision).expect("serialize"),
        json!({"denied": {"rejection": "denied"}}),
    );
}

#[test]
fn unknown_variants_are_still_rejected() {
    let err = serde_json::from_value::<ReviewDecision>(json!("shipped")).expect_err("unknown");

    assert!(
        err.to_string().contains("unknown variant"),
        "unexpected error: {err}"
    );
}
