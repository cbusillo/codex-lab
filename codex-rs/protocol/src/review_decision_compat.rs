//! Backward-compatible deserialization for [`ReviewDecision`].
//!
//! `Denied` used to be a unit variant that serialized as the bare string
//! `"denied"`. It now carries a `rejection` string and serializes as
//! `{"denied": {"rejection": "..."}}`. Older app-server v1 clients and MCP
//! elicitation clients still send the unit form, so deserialization keeps
//! accepting it while serialization only ever emits the current form.

use std::fmt;

use serde::Deserialize;
use serde::Deserializer;
use serde::de;
use serde::de::MapAccess;
use serde::de::Visitor;

use crate::protocol::ExecPolicyAmendment;
use crate::protocol::NetworkPolicyAmendment;
use crate::protocol::ReviewDecision;

/// Rejection recorded when a legacy client sends the unit-form `"denied"`.
const LEGACY_DENIED_REJECTION: &str = "denied";

const VARIANTS: &[&str] = &[
    "approved",
    "approved_execpolicy_amendment",
    "approved_for_session",
    "approved_mcp_policy_amendment",
    "network_policy_amendment",
    "denied",
    "timed_out",
    "abort",
];

impl<'de> Deserialize<'de> for ReviewDecision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ReviewDecisionVisitor)
    }
}

#[derive(Deserialize)]
struct DeniedPayload {
    rejection: String,
}

#[derive(Deserialize)]
struct ApprovedExecpolicyAmendmentPayload {
    proposed_execpolicy_amendment: ExecPolicyAmendment,
}

#[derive(Deserialize)]
struct NetworkPolicyAmendmentPayload {
    network_policy_amendment: NetworkPolicyAmendment,
}

struct ReviewDecisionVisitor;

impl<'de> Visitor<'de> for ReviewDecisionVisitor {
    type Value = ReviewDecision;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a review decision variant name or a single-key object")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match value {
            "approved" => Ok(ReviewDecision::Approved),
            "approved_for_session" => Ok(ReviewDecision::ApprovedForSession),
            "approved_mcp_policy_amendment" => Ok(ReviewDecision::ApprovedMcpPolicyAmendment),
            // Legacy unit form, emitted before `Denied` carried a rejection.
            "denied" => Ok(ReviewDecision::denied(LEGACY_DENIED_REJECTION)),
            "timed_out" => Ok(ReviewDecision::TimedOut),
            "abort" => Ok(ReviewDecision::Abort),
            other => Err(de::Error::unknown_variant(other, VARIANTS)),
        }
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let Some(variant) = map.next_key::<String>()? else {
            return Err(de::Error::invalid_length(0, &self));
        };

        let decision = match variant.as_str() {
            "approved" => {
                map.next_value::<()>()?;
                ReviewDecision::Approved
            }
            "approved_execpolicy_amendment" => {
                let payload = map.next_value::<ApprovedExecpolicyAmendmentPayload>()?;
                ReviewDecision::ApprovedExecpolicyAmendment {
                    proposed_execpolicy_amendment: payload.proposed_execpolicy_amendment,
                }
            }
            "approved_for_session" => {
                map.next_value::<()>()?;
                ReviewDecision::ApprovedForSession
            }
            "approved_mcp_policy_amendment" => {
                map.next_value::<()>()?;
                ReviewDecision::ApprovedMcpPolicyAmendment
            }
            "network_policy_amendment" => {
                let payload = map.next_value::<NetworkPolicyAmendmentPayload>()?;
                ReviewDecision::NetworkPolicyAmendment {
                    network_policy_amendment: payload.network_policy_amendment,
                }
            }
            "denied" => {
                let payload = map.next_value::<DeniedPayload>()?;
                ReviewDecision::denied(payload.rejection)
            }
            "timed_out" => {
                map.next_value::<()>()?;
                ReviewDecision::TimedOut
            }
            "abort" => {
                map.next_value::<()>()?;
                ReviewDecision::Abort
            }
            other => return Err(de::Error::unknown_variant(other, VARIANTS)),
        };

        if map.next_key::<de::IgnoredAny>()?.is_some() {
            return Err(de::Error::invalid_length(2, &self));
        }

        Ok(decision)
    }
}

#[cfg(test)]
#[path = "review_decision_compat_tests.rs"]
mod tests;
