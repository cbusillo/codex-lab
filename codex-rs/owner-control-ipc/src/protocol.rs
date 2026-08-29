use codex_owner_control_contract::ApprovalRequest;
use codex_owner_control_contract::ChannelBindingRecord;
use codex_owner_control_contract::ServerReviewPayload;
use serde::Deserialize;
use serde::Serialize;

pub const OWNER_CONTROL_IPC_PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChallengeMaterial {
    pub approval_request: ApprovalRequest,
    pub channel_binding: ChannelBindingRecord,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum OwnerControlIpcOperation {
    InspectChallenge(ChallengeMaterial),
    ConfirmChallenge(ChallengeMaterial),
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlIpcRequest {
    pub protocol_version: u8,
    pub operation: OwnerControlIpcOperation,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IpcFailureCode {
    FrameTooLarge,
    GestureUnavailable,
    InvalidContractInput,
    MalformedRequest,
    UnsupportedVersion,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnerControlIpcOutcome {
    ReviewAvailable {
        review: ServerReviewPayload,
        approval_request_digest: String,
        channel_binding_digest: String,
    },
    Rejected {
        code: IpcFailureCode,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlIpcResponse {
    pub protocol_version: u8,
    pub outcome: OwnerControlIpcOutcome,
}

impl OwnerControlIpcResponse {
    pub(crate) fn rejected(code: IpcFailureCode) -> Self {
        Self {
            protocol_version: OWNER_CONTROL_IPC_PROTOCOL_VERSION,
            outcome: OwnerControlIpcOutcome::Rejected { code },
        }
    }
}
