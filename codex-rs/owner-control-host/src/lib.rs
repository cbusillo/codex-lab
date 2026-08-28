//! Inert, fail-closed host logic for owner-control confirmation envelopes.
//!
//! This crate deliberately contains no I/O or authorization integration. Hosts
//! provide time, replay storage, and signature custody through narrow seams.

use std::error::Error;
use std::fmt;

use codex_owner_control_contract::ChallengeResponse;
use codex_owner_control_contract::ChannelBindingRecord;
use codex_owner_control_contract::OWNER_CONTROL_SCHEMA_VERSION;
use codex_owner_control_contract::OWNER_CONTROL_SIGNATURE_ALGORITHM;
use codex_owner_control_contract::OwnerControlConfirmationEnvelope;
use codex_owner_control_contract::approval_request_digest;
use codex_owner_control_contract::challenge_response_digest;
use codex_owner_control_contract::channel_binding_sha256;
use codex_owner_control_contract::signature_payload_bytes;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Supplies the current instant without choosing a runtime or clock source.
pub trait OwnerClock {
    fn now(&self) -> OffsetDateTime;
}

/// Signs canonical owner-control signature payload bytes without exposing keys.
pub trait OwnerSigningCustody {
    fn sign_owner_confirmation(&self, payload: &[u8]) -> Result<String, OwnerSigningFailure>;
}

/// A custody implementation failed without disclosing its internal state.
pub struct OwnerSigningFailure(());

impl OwnerSigningFailure {
    pub fn unavailable() -> Self {
        Self(())
    }
}

impl fmt::Debug for OwnerSigningFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnerSigningFailure")
    }
}

impl fmt::Display for OwnerSigningFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("owner signing custody is unavailable")
    }
}

impl Error for OwnerSigningFailure {}

/// Atomically rejects prior confirmations and records a newly consumed challenge.
pub trait OwnerControlReplayStore {
    fn check_and_insert(&mut self, challenge_digest: &str) -> Result<(), ReplayStoreFailure>;
}

/// A replay store could not reserve a challenge without exposing storage details.
pub struct ReplayStoreFailure(());

impl ReplayStoreFailure {
    pub fn rejected() -> Self {
        Self(())
    }
}

impl fmt::Debug for ReplayStoreFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReplayStoreFailure")
    }
}

impl fmt::Display for ReplayStoreFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("owner confirmation replay reservation failed")
    }
}

impl Error for ReplayStoreFailure {}

/// Fail-closed outcomes that do not disclose signature or custody internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationError {
    InvalidContractInput,
    OwnerMismatch,
    GestureMismatch,
    ChallengeExpired,
    ChallengeOutsideSession,
    ReplayRejected,
    CustodyUnavailable,
    InvalidCustodySignature,
}

impl fmt::Display for ConfirmationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidContractInput => "owner-control contract input is invalid",
            Self::OwnerMismatch => "owner identity does not match the confirmation challenge",
            Self::GestureMismatch => "owner gesture does not match the confirmation challenge",
            Self::ChallengeExpired => "owner-control challenge has expired",
            Self::ChallengeOutsideSession => {
                "owner-control challenge is outside its channel session"
            }
            Self::ReplayRejected => "owner-control challenge replay was rejected",
            Self::CustodyUnavailable => "owner signing custody is unavailable",
            Self::InvalidCustodySignature => "owner signing custody returned an invalid signature",
        };
        formatter.write_str(message)
    }
}

impl Error for ConfirmationError {}

/// A server-authored review presented without a caller-provided mutation path.
pub struct PresentedOwnerConfirmation {
    challenge_response: ChallengeResponse,
    channel_binding: ChannelBindingRecord,
    challenge_digest: String,
    channel_binding_digest: String,
    request_digest: String,
}

impl PresentedOwnerConfirmation {
    pub fn from_values(
        challenge_response: Value,
        channel_binding: Value,
    ) -> Result<Self, ConfirmationError> {
        let challenge_response = ChallengeResponse::from_value(challenge_response)
            .map_err(|_| ConfirmationError::InvalidContractInput)?;
        let channel_binding = ChannelBindingRecord::from_value(channel_binding)
            .map_err(|_| ConfirmationError::InvalidContractInput)?;
        Self::new(challenge_response, channel_binding)
    }

    pub fn new(
        challenge_response: ChallengeResponse,
        channel_binding: ChannelBindingRecord,
    ) -> Result<Self, ConfirmationError> {
        validate_pair(&challenge_response, &channel_binding)?;
        Ok(Self {
            challenge_digest: challenge_response_digest(&challenge_response)
                .map_err(|_| ConfirmationError::InvalidContractInput)?,
            channel_binding_digest: channel_binding_sha256(&channel_binding)
                .map_err(|_| ConfirmationError::InvalidContractInput)?,
            request_digest: challenge_response.approval_request.request_digest.clone(),
            challenge_response,
            channel_binding,
        })
    }

    pub fn review(&self) -> &codex_owner_control_contract::ServerReviewPayload {
        &self.challenge_response.approval_request.server_review
    }

    pub fn acknowledge_owner(
        self,
        owner_github_id: i64,
    ) -> Result<(ConfirmationFlow, OwnerGesture), ConfirmationError> {
        if owner_github_id != self.challenge_response.approval_request.owner_github_id {
            return Err(ConfirmationError::OwnerMismatch);
        }
        let gesture = OwnerGesture {
            challenge_digest: self.challenge_digest.clone(),
            owner_github_id,
        };
        Ok((
            ConfirmationFlow {
                challenge_response: self.challenge_response,
                channel_binding: self.channel_binding,
                challenge_digest: self.challenge_digest,
                channel_binding_digest: self.channel_binding_digest,
                request_digest: self.request_digest,
            },
            gesture,
        ))
    }
}

/// A private, consuming acknowledgement bound to one exact challenge digest.
pub struct OwnerGesture {
    challenge_digest: String,
    owner_github_id: i64,
}

impl fmt::Debug for OwnerGesture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnerGesture([redacted])")
    }
}

/// A confirmation flow that can be consumed exactly once with its matching gesture.
pub struct ConfirmationFlow {
    challenge_response: ChallengeResponse,
    channel_binding: ChannelBindingRecord,
    challenge_digest: String,
    channel_binding_digest: String,
    request_digest: String,
}

impl ConfirmationFlow {
    pub fn confirm(
        self,
        gesture: OwnerGesture,
        clock: &impl OwnerClock,
        custody: &impl OwnerSigningCustody,
        replay_store: &mut impl OwnerControlReplayStore,
    ) -> Result<ConfirmedOwnerControlEnvelope, ConfirmationError> {
        self.recheck(&gesture, clock.now())?;
        replay_store
            .check_and_insert(&self.challenge_digest)
            .map_err(|_| ConfirmationError::ReplayRejected)?;
        let payload = signature_payload_bytes(&self.challenge_response)
            .map_err(|_| ConfirmationError::InvalidContractInput)?;
        let signature = custody
            .sign_owner_confirmation(&payload)
            .map_err(|_| ConfirmationError::CustodyUnavailable)?;
        let envelope = OwnerControlConfirmationEnvelope {
            schema_version: OWNER_CONTROL_SCHEMA_VERSION,
            channel_binding: self.channel_binding,
            challenge_response: self.challenge_response,
            signature_algorithm: OWNER_CONTROL_SIGNATURE_ALGORITHM.to_owned(),
            signature,
        };
        envelope
            .validate()
            .map_err(|_| ConfirmationError::InvalidCustodySignature)?;
        Ok(ConfirmedOwnerControlEnvelope(envelope))
    }

    fn recheck(
        &self,
        gesture: &OwnerGesture,
        now: OffsetDateTime,
    ) -> Result<(), ConfirmationError> {
        validate_pair(&self.challenge_response, &self.channel_binding)?;
        if self.challenge_response.approval_request.request_digest != self.request_digest {
            return Err(ConfirmationError::InvalidContractInput);
        }
        if challenge_response_digest(&self.challenge_response)
            .map_err(|_| ConfirmationError::InvalidContractInput)?
            != self.challenge_digest
            || gesture.challenge_digest != self.challenge_digest
        {
            return Err(ConfirmationError::GestureMismatch);
        }
        if channel_binding_sha256(&self.channel_binding)
            .map_err(|_| ConfirmationError::InvalidContractInput)?
            != self.channel_binding_digest
            || self.challenge_response.channel_binding_sha256 != self.channel_binding_digest
        {
            return Err(ConfirmationError::InvalidContractInput);
        }
        let owner_github_id = self.challenge_response.approval_request.owner_github_id;
        if owner_github_id != self.channel_binding.owner_github_id
            || owner_github_id != gesture.owner_github_id
        {
            return Err(ConfirmationError::OwnerMismatch);
        }
        let request_issued_at = parse_time(&self.challenge_response.approval_request.issued_at)?;
        let request_expires_at = parse_time(&self.challenge_response.approval_request.expires_at)?;
        let session_issued_at = parse_time(&self.channel_binding.session_issued_at)?;
        let session_expires_at = parse_time(&self.channel_binding.session_expires_at)?;
        if now < request_issued_at || now > request_expires_at {
            return Err(ConfirmationError::ChallengeExpired);
        }
        if now < session_issued_at || now > session_expires_at {
            return Err(ConfirmationError::ChallengeOutsideSession);
        }
        Ok(())
    }
}

/// A complete contract envelope whose debug representation redacts its signature.
pub struct ConfirmedOwnerControlEnvelope(OwnerControlConfirmationEnvelope);

impl ConfirmedOwnerControlEnvelope {
    pub fn envelope(&self) -> &OwnerControlConfirmationEnvelope {
        &self.0
    }

    pub fn into_envelope(self) -> OwnerControlConfirmationEnvelope {
        self.0
    }
}

impl fmt::Debug for ConfirmedOwnerControlEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConfirmedOwnerControlEnvelope { signature: [redacted] }")
    }
}

fn validate_pair(
    challenge_response: &ChallengeResponse,
    channel_binding: &ChannelBindingRecord,
) -> Result<(), ConfirmationError> {
    challenge_response
        .validate()
        .map_err(|_| ConfirmationError::InvalidContractInput)?;
    channel_binding
        .validate()
        .map_err(|_| ConfirmationError::InvalidContractInput)?;
    if challenge_response.schema_version != OWNER_CONTROL_SCHEMA_VERSION
        || challenge_response.approval_request.schema_version != OWNER_CONTROL_SCHEMA_VERSION
        || challenge_response
            .approval_request
            .server_review
            .schema_version
            != OWNER_CONTROL_SCHEMA_VERSION
        || channel_binding.schema_version != OWNER_CONTROL_SCHEMA_VERSION
        || challenge_response.approval_request.owner_github_id != channel_binding.owner_github_id
        || challenge_response.channel_binding_sha256
            != channel_binding_sha256(channel_binding)
                .map_err(|_| ConfirmationError::InvalidContractInput)?
        || challenge_response.approval_request_digest
            != approval_request_digest(&challenge_response.approval_request)
                .map_err(|_| ConfirmationError::InvalidContractInput)?
    {
        return Err(ConfirmationError::InvalidContractInput);
    }
    Ok(())
}

fn parse_time(value: &str) -> Result<OffsetDateTime, ConfirmationError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| ConfirmationError::InvalidContractInput)
}

#[cfg(test)]
mod tests;
