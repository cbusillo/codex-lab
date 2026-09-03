//! Inert, fail-closed host logic for owner-control confirmation envelopes.
//!
//! This crate deliberately contains no I/O or authorization integration. Hosts
//! provide time, replay storage, and signature custody through narrow seams.

use std::error::Error;
use std::fmt;

use codex_owner_control_contract::ApprovalRequest;
use codex_owner_control_contract::ChallengeResponse;
use codex_owner_control_contract::ChannelBindingRecord;
use codex_owner_control_contract::Decision;
use codex_owner_control_contract::OWNER_CONTROL_SCHEMA_VERSION;
use codex_owner_control_contract::OWNER_CONTROL_SIGNATURE_ALGORITHM;
use codex_owner_control_contract::OwnerControlAuthorityState;
use codex_owner_control_contract::OwnerControlConfirmationEnvelope;
use codex_owner_control_contract::OwnerControlGestureSourceClaim;
use codex_owner_control_contract::OwnerControlHostPrincipalClaim;
use codex_owner_control_contract::OwnerControlKeyCustodyClaim;
use codex_owner_control_contract::OwnerControlPrincipalSeparationClaim;
use codex_owner_control_contract::OwnerControlProvenanceTier;
use codex_owner_control_contract::OwnerControlServerObservedCorroboration;
use codex_owner_control_contract::ValidationError;
use codex_owner_control_contract::approval_request_digest;
use codex_owner_control_contract::canonical_json_sha256;
use codex_owner_control_contract::channel_binding_sha256;
use codex_owner_control_contract::derive_owner_control_provenance_tier;
use codex_owner_control_contract::is_published_owner_control_synthetic_public_key;
use codex_owner_control_contract::owner_control_host_principal_claim_sha256;
use codex_owner_control_contract::signature_payload_bytes;
use codex_owner_control_contract::verify_confirmation_signature_proof;
use serde_json::Value;
use time::OffsetDateTime;
use time::UtcOffset;
use time::format_description::well_known::Rfc3339;

/// A sealed read-only view of capabilities this host can currently observe.
///
/// The current host has no separate security domain, no custody attestation,
/// and no gesture source. Caller-declared identifiers are retained only for
/// enrollment binding and never raise the derived trust tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedOwnerControlHost {
    principal_claim: OwnerControlHostPrincipalClaim,
    server_observed_corroboration: OwnerControlServerObservedCorroboration,
    provenance_tier: OwnerControlProvenanceTier,
}

impl ObservedOwnerControlHost {
    pub fn current(
        host_instance_id: impl Into<String>,
        principal_id: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let principal_claim = OwnerControlHostPrincipalClaim {
            schema_version:
                codex_owner_control_contract::OWNER_CONTROL_ENROLLMENT_PROVENANCE_SCHEMA_VERSION,
            host_instance_id: host_instance_id.into(),
            principal_id: principal_id.into(),
            principal_separation: OwnerControlPrincipalSeparationClaim::NotClaimed,
            key_custody: OwnerControlKeyCustodyClaim::NotClaimed,
            gesture_source: OwnerControlGestureSourceClaim::NotClaimed,
        };
        principal_claim.validate()?;
        let server_observed_corroboration = OwnerControlServerObservedCorroboration::None;
        let provenance_tier = derive_owner_control_provenance_tier(server_observed_corroboration);
        Ok(Self {
            principal_claim,
            server_observed_corroboration,
            provenance_tier,
        })
    }

    pub fn principal_claim(&self) -> &OwnerControlHostPrincipalClaim {
        &self.principal_claim
    }

    pub fn server_observed_corroboration(&self) -> OwnerControlServerObservedCorroboration {
        self.server_observed_corroboration
    }

    pub fn provenance_tier(&self) -> OwnerControlProvenanceTier {
        self.provenance_tier
    }

    pub fn authority_state(&self) -> OwnerControlAuthorityState {
        OwnerControlAuthorityState::Inert
    }

    pub fn authorizes_execution(&self) -> bool {
        false
    }

    pub fn bind_channel(
        &self,
        channel_binding: ChannelBindingRecord,
    ) -> Result<OwnerControlEnrollmentIntent, ValidationError> {
        channel_binding.validate()?;
        if is_published_owner_control_synthetic_public_key(&channel_binding.owner_public_key)? {
            return Err(ValidationError::Rule {
                rule: "published owner-control conformance keys cannot be enrolled",
            });
        }
        Ok(OwnerControlEnrollmentIntent {
            channel_binding_sha256: channel_binding_sha256(&channel_binding)?,
            principal_claim_sha256: owner_control_host_principal_claim_sha256(
                &self.principal_claim,
            )?,
            channel_binding,
            observed_host: self.clone(),
        })
    }
}

/// An inert, data-only request to bind one observed host to one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerControlEnrollmentIntent {
    channel_binding: ChannelBindingRecord,
    channel_binding_sha256: String,
    observed_host: ObservedOwnerControlHost,
    principal_claim_sha256: String,
}

impl OwnerControlEnrollmentIntent {
    pub fn channel_binding(&self) -> &ChannelBindingRecord {
        &self.channel_binding
    }

    pub fn channel_binding_sha256(&self) -> &str {
        &self.channel_binding_sha256
    }

    pub fn observed_host(&self) -> &ObservedOwnerControlHost {
        &self.observed_host
    }

    pub fn principal_claim_sha256(&self) -> &str {
        &self.principal_claim_sha256
    }

    pub fn authorizes_execution(&self) -> bool {
        false
    }
}

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
    approval_request: ApprovalRequest,
    channel_binding: ChannelBindingRecord,
    approval_request_digest: String,
    channel_binding_digest: String,
    gesture_digest: String,
}

impl PresentedOwnerConfirmation {
    pub fn from_values(
        approval_request: Value,
        channel_binding: Value,
    ) -> Result<Self, ConfirmationError> {
        let approval_request = ApprovalRequest::from_value(approval_request)
            .map_err(|_| ConfirmationError::InvalidContractInput)?;
        let channel_binding = ChannelBindingRecord::from_value(channel_binding)
            .map_err(|_| ConfirmationError::InvalidContractInput)?;
        Self::new(approval_request, channel_binding)
    }

    pub fn new(
        approval_request: ApprovalRequest,
        channel_binding: ChannelBindingRecord,
    ) -> Result<Self, ConfirmationError> {
        validate_pair(&approval_request, &channel_binding)?;
        let approval_request_digest = approval_request_digest(&approval_request)
            .map_err(|_| ConfirmationError::InvalidContractInput)?;
        let channel_binding_digest = channel_binding_sha256(&channel_binding)
            .map_err(|_| ConfirmationError::InvalidContractInput)?;
        Ok(Self {
            gesture_digest: exact_gesture_digest(
                &approval_request_digest,
                &channel_binding_digest,
            )?,
            approval_request_digest,
            channel_binding_digest,
            approval_request,
            channel_binding,
        })
    }

    pub fn review(&self) -> &codex_owner_control_contract::ServerReviewPayload {
        &self.approval_request.server_review
    }

    pub fn acknowledge_owner(self) -> (ConfirmationFlow, OwnerGesture) {
        let gesture = OwnerGesture {
            gesture_digest: self.gesture_digest.clone(),
        };
        (
            ConfirmationFlow {
                approval_request: self.approval_request,
                channel_binding: self.channel_binding,
                approval_request_digest: self.approval_request_digest,
                channel_binding_digest: self.channel_binding_digest,
                gesture_digest: self.gesture_digest,
            },
            gesture,
        )
    }
}

/// A private, consuming acknowledgement bound to one exact challenge digest.
pub struct OwnerGesture {
    gesture_digest: String,
}

impl fmt::Debug for OwnerGesture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnerGesture([redacted])")
    }
}

/// A confirmation flow that can be consumed exactly once with its matching gesture.
pub struct ConfirmationFlow {
    approval_request: ApprovalRequest,
    channel_binding: ChannelBindingRecord,
    approval_request_digest: String,
    channel_binding_digest: String,
    gesture_digest: String,
}

impl ConfirmationFlow {
    pub fn confirm(
        self,
        gesture: OwnerGesture,
        clock: &impl OwnerClock,
        custody: &impl OwnerSigningCustody,
        replay_store: &mut impl OwnerControlReplayStore,
    ) -> Result<ConfirmedOwnerControlEnvelope, ConfirmationError> {
        let now = clock.now();
        self.recheck(&gesture, now)?;
        replay_store
            .check_and_insert(&self.gesture_digest)
            .map_err(|_| ConfirmationError::ReplayRejected)?;
        let challenge_response = ChallengeResponse {
            schema_version: OWNER_CONTROL_SCHEMA_VERSION,
            approval_request: self.approval_request,
            approval_request_digest: self.approval_request_digest,
            decision: Decision::Approved,
            channel_binding_sha256: self.channel_binding_digest,
            confirmed_at: canonical_timestamp(now)?,
        };
        challenge_response
            .validate()
            .map_err(|_| ConfirmationError::InvalidContractInput)?;
        let payload = signature_payload_bytes(&challenge_response)
            .map_err(|_| ConfirmationError::InvalidContractInput)?;
        let signature = custody
            .sign_owner_confirmation(&payload)
            .map_err(|_| ConfirmationError::CustodyUnavailable)?;
        let envelope = OwnerControlConfirmationEnvelope {
            schema_version: OWNER_CONTROL_SCHEMA_VERSION,
            channel_binding: self.channel_binding,
            challenge_response,
            signature_algorithm: OWNER_CONTROL_SIGNATURE_ALGORITHM.to_owned(),
            signature,
        };
        envelope
            .validate()
            .map_err(|_| ConfirmationError::InvalidCustodySignature)?;
        if !verify_confirmation_signature_proof(&envelope) {
            return Err(ConfirmationError::InvalidCustodySignature);
        }
        Ok(ConfirmedOwnerControlEnvelope(envelope))
    }

    fn recheck(
        &self,
        gesture: &OwnerGesture,
        now: OffsetDateTime,
    ) -> Result<(), ConfirmationError> {
        validate_pair(&self.approval_request, &self.channel_binding)?;
        if approval_request_digest(&self.approval_request)
            .map_err(|_| ConfirmationError::InvalidContractInput)?
            != self.approval_request_digest
        {
            return Err(ConfirmationError::InvalidContractInput);
        }
        if exact_gesture_digest(&self.approval_request_digest, &self.channel_binding_digest)?
            != self.gesture_digest
            || gesture.gesture_digest != self.gesture_digest
        {
            return Err(ConfirmationError::GestureMismatch);
        }
        if channel_binding_sha256(&self.channel_binding)
            .map_err(|_| ConfirmationError::InvalidContractInput)?
            != self.channel_binding_digest
        {
            return Err(ConfirmationError::InvalidContractInput);
        }
        if self.approval_request.owner_github_id != self.channel_binding.owner_github_id {
            return Err(ConfirmationError::OwnerMismatch);
        }
        let request_issued_at = parse_time(&self.approval_request.issued_at)?;
        let request_expires_at = parse_time(&self.approval_request.expires_at)?;
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
    approval_request: &ApprovalRequest,
    channel_binding: &ChannelBindingRecord,
) -> Result<(), ConfirmationError> {
    approval_request
        .validate()
        .map_err(|_| ConfirmationError::InvalidContractInput)?;
    channel_binding
        .validate()
        .map_err(|_| ConfirmationError::InvalidContractInput)?;
    if approval_request.schema_version != OWNER_CONTROL_SCHEMA_VERSION
        || approval_request.server_review.schema_version != OWNER_CONTROL_SCHEMA_VERSION
        || channel_binding.schema_version != OWNER_CONTROL_SCHEMA_VERSION
        || approval_request.owner_github_id != channel_binding.owner_github_id
    {
        return Err(ConfirmationError::InvalidContractInput);
    }
    let request_issued_at = parse_time(&approval_request.issued_at)?;
    let request_expires_at = parse_time(&approval_request.expires_at)?;
    let session_issued_at = parse_time(&channel_binding.session_issued_at)?;
    let session_expires_at = parse_time(&channel_binding.session_expires_at)?;
    if request_issued_at < session_issued_at || request_expires_at > session_expires_at {
        return Err(ConfirmationError::ChallengeOutsideSession);
    }
    Ok(())
}

fn exact_gesture_digest(
    approval_request_digest: &str,
    channel_binding_digest: &str,
) -> Result<String, ConfirmationError> {
    canonical_json_sha256(&serde_json::json!({
        "approval_request_digest": approval_request_digest,
        "channel_binding_sha256": channel_binding_digest,
    }))
    .map_err(|_| ConfirmationError::InvalidContractInput)
}

fn canonical_timestamp(value: OffsetDateTime) -> Result<String, ConfirmationError> {
    let value = value.to_offset(UtcOffset::UTC);
    let year = value.year();
    if !(1..=9999).contains(&year) {
        return Err(ConfirmationError::InvalidContractInput);
    }
    let month = value.month() as u8;
    let day = value.day();
    let hour = value.hour();
    let minute = value.minute();
    let second = value.second();
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+00:00"
    ))
}

fn parse_time(value: &str) -> Result<OffsetDateTime, ConfirmationError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| ConfirmationError::InvalidContractInput)
}

#[cfg(test)]
mod tests;
