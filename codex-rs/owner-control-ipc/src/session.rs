use std::error::Error;
use std::fmt;

#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::io::Write;

use codex_owner_control_contract::ServerReviewPayload;
use codex_owner_control_contract::approval_request_digest;
use codex_owner_control_contract::channel_binding_sha256;
use codex_owner_control_host::PresentedOwnerConfirmation;

use crate::ChallengeMaterial;
use crate::IpcFailureCode;
use crate::OWNER_CONTROL_IPC_PROTOCOL_VERSION;
use crate::OwnerControlIpcOperation;
use crate::OwnerControlIpcOutcome;
use crate::OwnerControlIpcRequest;
use crate::OwnerControlIpcResponse;

#[cfg(unix)]
use crate::FrameError;
#[cfg(unix)]
use crate::framing::read_frame;
#[cfg(unix)]
use crate::framing::write_frame;

mod private {
    pub trait Sealed {}
}

/// Supplies a distinct local owner gesture without accepting gesture material
/// from the IPC caller.
pub trait OwnerGestureSource: private::Sealed {
    fn request_owner_gesture(&self, review: &ServerReviewPayload)
    -> Result<(), GestureUnavailable>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DenyAllGestureSource;

impl private::Sealed for DenyAllGestureSource {}

impl OwnerGestureSource for DenyAllGestureSource {
    fn request_owner_gesture(
        &self,
        _review: &ServerReviewPayload,
    ) -> Result<(), GestureUnavailable> {
        Err(GestureUnavailable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GestureUnavailable;

impl fmt::Display for GestureUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("owner gesture is unavailable")
    }
}

impl Error for GestureUnavailable {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    ReadFailed,
    WriteFailed,
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ReadFailed => "owner-control IPC request read failed",
            Self::WriteFailed => "owner-control IPC response write failed",
        };
        formatter.write_str(message)
    }
}

impl Error for SessionError {}

pub fn handle_request(
    request: OwnerControlIpcRequest,
    gesture_source: &impl OwnerGestureSource,
) -> OwnerControlIpcResponse {
    if request.protocol_version != OWNER_CONTROL_IPC_PROTOCOL_VERSION {
        return OwnerControlIpcResponse::rejected(IpcFailureCode::UnsupportedVersion);
    }
    match request.operation {
        OwnerControlIpcOperation::InspectChallenge(material) => present_review(material),
        OwnerControlIpcOperation::ConfirmChallenge(material) => {
            let response = present_review(material);
            let OwnerControlIpcOutcome::ReviewAvailable { review, .. } = &response.outcome else {
                return response;
            };
            let _ = gesture_source.request_owner_gesture(review);
            OwnerControlIpcResponse::rejected(IpcFailureCode::GestureUnavailable)
        }
    }
}

#[cfg(unix)]
pub(crate) fn serve_stream(
    stream: &mut (impl Read + Write),
    gesture_source: &impl OwnerGestureSource,
) -> Result<(), SessionError> {
    let response = match read_frame::<OwnerControlIpcRequest>(stream) {
        Ok(request) => handle_request(request, gesture_source),
        Err(FrameError::FrameTooLarge) => {
            OwnerControlIpcResponse::rejected(IpcFailureCode::FrameTooLarge)
        }
        Err(FrameError::EmptyFrame | FrameError::InvalidJson) => {
            OwnerControlIpcResponse::rejected(IpcFailureCode::MalformedRequest)
        }
        Err(FrameError::Io) => return Err(SessionError::ReadFailed),
    };
    write_frame(stream, &response).map_err(|_| SessionError::WriteFailed)
}

fn present_review(material: ChallengeMaterial) -> OwnerControlIpcResponse {
    let approval_request = material.approval_request;
    let channel_binding = material.channel_binding;
    let Ok(presentation) =
        PresentedOwnerConfirmation::new(approval_request.clone(), channel_binding.clone())
    else {
        return OwnerControlIpcResponse::rejected(IpcFailureCode::InvalidContractInput);
    };
    let Ok(approval_request_digest) = approval_request_digest(&approval_request) else {
        return OwnerControlIpcResponse::rejected(IpcFailureCode::InvalidContractInput);
    };
    let Ok(channel_binding_digest) = channel_binding_sha256(&channel_binding) else {
        return OwnerControlIpcResponse::rejected(IpcFailureCode::InvalidContractInput);
    };
    OwnerControlIpcResponse {
        protocol_version: OWNER_CONTROL_IPC_PROTOCOL_VERSION,
        outcome: OwnerControlIpcOutcome::ReviewAvailable {
            review: presentation.review().clone(),
            approval_request_digest,
            channel_binding_digest,
        },
    }
}
