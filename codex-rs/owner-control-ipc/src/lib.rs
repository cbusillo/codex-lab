//! Inert local IPC for presenting validated owner-control reviews.
//!
//! No request or response in this crate can carry or produce a signed owner
//! confirmation envelope. Runtime integration and principal isolation remain
//! explicitly out of scope.

mod endpoint;
mod framing;
mod protocol;
mod session;

pub use endpoint::EndpointError;
pub use endpoint::EndpointServeError;
pub use endpoint::OwnerControlEndpoint;
pub use framing::FrameError;
pub use framing::MAX_FRAME_BYTES;
pub use framing::read_frame;
pub use framing::write_frame;
pub use protocol::ChallengeMaterial;
pub use protocol::IpcFailureCode;
pub use protocol::OWNER_CONTROL_IPC_PROTOCOL_VERSION;
pub use protocol::OwnerControlIpcOperation;
pub use protocol::OwnerControlIpcOutcome;
pub use protocol::OwnerControlIpcRequest;
pub use protocol::OwnerControlIpcResponse;
pub use session::DenyAllGestureSource;
pub use session::GestureUnavailable;
pub use session::OwnerGestureSource;
pub use session::SessionError;
pub use session::handle_request;

#[cfg(test)]
mod tests;
