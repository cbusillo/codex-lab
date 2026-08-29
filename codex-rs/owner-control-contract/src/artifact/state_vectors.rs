use std::collections::BTreeSet;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use super::ArtifactError;
use super::OWNER_CONTROL_MAX_ATTEMPTS;
use super::OWNER_CONTROL_STATE_SCHEMA_VERSION;
use crate::ApprovalRequest;
use crate::ChannelBindingRecord;
use crate::DescriptorId;
use crate::OwnerControlConfirmationEnvelope;
use crate::canonical_json_bytes;
use crate::canonical_json_sha256;
use crate::model::parse_timestamp;
use crate::verify_confirmation_signature_proof;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlAuthorityState {
    Inert,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlVerifierMode {
    Shadow,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlChannelSessionStatus {
    Enrolled,
    Revoked,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlChallengeState {
    Issued,
    Consumed,
    Expired,
    Rejected,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlVerificationStatus {
    Verified,
    Rejected,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlRejectionReason {
    UnknownChannelSession,
    UnknownChallenge,
    ChannelSessionRevoked,
    ChannelSessionExpired,
    ChallengeChannelSessionMismatch,
    ChallengeExpired,
    ChallengeReplayed,
    StoredBindingMismatch,
    StoredApprovalRequestMismatch,
    SignatureInvalid,
    AttemptBudgetExhausted,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlTransitionReason {
    Expired,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlChannelSessionRecord {
    pub authority_state: OwnerControlAuthorityState,
    pub binding_json: String,
    pub binding_sha256: String,
    pub channel_session_id: String,
    pub enrolled_at: String,
    pub owner_github_id: i64,
    pub revoked_at: Option<String>,
    pub schema_version: u8,
    pub status: OwnerControlChannelSessionStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlChallengeRecord {
    pub approval_request_json: String,
    pub approval_request_sha256: String,
    pub attempt_count: u8,
    pub authority_state: OwnerControlAuthorityState,
    pub binding_json: String,
    pub binding_sha256: String,
    pub challenge_id: String,
    pub challenge_nonce: String,
    pub channel_session_id: String,
    pub consumed_at: Option<String>,
    pub descriptor_id: DescriptorId,
    pub expires_at: String,
    pub issued_at: String,
    pub operation_id: String,
    pub owner_github_id: i64,
    pub schema_version: u8,
    pub state: OwnerControlChallengeState,
    pub terminal_event_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationStateExpectation {
    pub authority_state: OwnerControlAuthorityState,
    pub authorizes_execution: bool,
    pub consume_challenge: bool,
    pub rejection_reason: Option<OwnerControlRejectionReason>,
    pub resulting_challenge_state: OwnerControlChallengeState,
    pub verification_status: OwnerControlVerificationStatus,
    pub verifier_mode: OwnerControlVerifierMode,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerificationStateVector {
    pub channel_session: Option<OwnerControlChannelSessionRecord>,
    pub confirmation_envelope: Value,
    pub expected: VerificationStateExpectation,
    pub issued_challenge: Option<OwnerControlChallengeRecord>,
    pub name: String,
    pub observed_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChallengeLifecycleEvent {
    pub approval_request_sha256: String,
    pub authority_state: OwnerControlAuthorityState,
    pub authorizes_execution: bool,
    pub binding_sha256: String,
    pub challenge_expires_at: String,
    pub challenge_id: String,
    pub challenge_nonce: String,
    pub channel_session_id: String,
    pub event_id: String,
    pub from_state: OwnerControlChallengeState,
    pub occurred_at: String,
    pub operation_id: String,
    pub schema_version: u8,
    pub to_state: OwnerControlChallengeState,
    pub transition_reason: OwnerControlTransitionReason,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChallengeLifecycleVector {
    pub expected_lifecycle_event: ChallengeLifecycleEvent,
    pub expected_terminalized_challenge: OwnerControlChallengeRecord,
    pub issued_challenge: OwnerControlChallengeRecord,
    pub name: String,
    pub observed_at: String,
}

pub(super) fn validate_verification_state_vectors(
    vectors: &[VerificationStateVector],
) -> Result<(), ArtifactError> {
    let expected_vectors = [
        (
            "verified",
            OwnerControlVerificationStatus::Verified,
            None,
            true,
            OwnerControlChallengeState::Consumed,
        ),
        (
            "unknown-channel-session",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::UnknownChannelSession),
            false,
            OwnerControlChallengeState::Rejected,
        ),
        (
            "unknown-challenge",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::UnknownChallenge),
            false,
            OwnerControlChallengeState::Rejected,
        ),
        (
            "channel-session-revoked",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::ChannelSessionRevoked),
            false,
            OwnerControlChallengeState::Rejected,
        ),
        (
            "channel-session-expired",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::ChannelSessionExpired),
            false,
            OwnerControlChallengeState::Expired,
        ),
        (
            "challenge-channel-session-mismatch",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::ChallengeChannelSessionMismatch),
            false,
            OwnerControlChallengeState::Rejected,
        ),
        (
            "challenge-expired",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::ChallengeExpired),
            false,
            OwnerControlChallengeState::Expired,
        ),
        (
            "challenge-replayed",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::ChallengeReplayed),
            false,
            OwnerControlChallengeState::Consumed,
        ),
        (
            "stored-binding-mismatch",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::StoredBindingMismatch),
            false,
            OwnerControlChallengeState::Issued,
        ),
        (
            "stored-approval-request-mismatch",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::StoredApprovalRequestMismatch),
            false,
            OwnerControlChallengeState::Issued,
        ),
        (
            "signature-invalid",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::SignatureInvalid),
            false,
            OwnerControlChallengeState::Issued,
        ),
        (
            "attempt-budget-exhausted",
            OwnerControlVerificationStatus::Rejected,
            Some(OwnerControlRejectionReason::AttemptBudgetExhausted),
            false,
            OwnerControlChallengeState::Rejected,
        ),
    ];
    if vectors.len() != expected_vectors.len() {
        return Err(invalid(
            "verification vectors must contain exactly twelve outcomes",
        ));
    }

    let mut names = BTreeSet::new();
    let mut reasons = BTreeSet::new();
    for vector in vectors {
        if !names.insert(vector.name.as_str()) {
            return Err(invalid("verification vector names must be unique"));
        }
        parse_timestamp(&vector.observed_at, "observed_at")?;
        if vector.expected.authorizes_execution {
            return Err(invalid("verification vectors must remain non-authorizing"));
        }
        if let Some(reason) = vector.expected.rejection_reason {
            reasons.insert(reason);
        }

        let Some((_, status, reason, consume, resulting_state)) = expected_vectors
            .iter()
            .find(|(name, ..)| *name == vector.name)
        else {
            return Err(ArtifactError::Invalid(format!(
                "unknown verification vector {}",
                vector.name
            )));
        };
        if vector.expected.verification_status != *status
            || vector.expected.rejection_reason != *reason
            || vector.expected.consume_challenge != *consume
            || vector.expected.resulting_challenge_state != *resulting_state
        {
            return Err(ArtifactError::Invalid(format!(
                "verification vector {} has inconsistent expected outcome",
                vector.name
            )));
        }

        validate_vector_record_presence(vector)?;
        if let Some(session) = &vector.channel_session {
            session.validate()?;
        }
        if let Some(challenge) = &vector.issued_challenge {
            challenge.validate()?;
        }

        let envelope =
            OwnerControlConfirmationEnvelope::from_value(vector.confirmation_envelope.clone())?;
        validate_named_state_condition(vector, &envelope)?;
        let proof_is_valid = verify_confirmation_signature_proof(&envelope);
        let proof_should_be_valid =
            vector.expected.rejection_reason != Some(OwnerControlRejectionReason::SignatureInvalid);
        if proof_is_valid != proof_should_be_valid {
            return Err(ArtifactError::Invalid(format!(
                "verification vector {} has unexpected signature proof validity",
                vector.name
            )));
        }
    }

    let expected_names = expected_vectors
        .iter()
        .map(|(name, ..)| *name)
        .collect::<BTreeSet<_>>();
    if names != expected_names || reasons != expected_rejection_reasons() {
        return Err(invalid(
            "verification vectors must cover the exact published names and rejection reasons",
        ));
    }
    Ok(())
}

pub(super) fn validate_challenge_lifecycle_vectors(
    vectors: &[ChallengeLifecycleVector],
) -> Result<(), ArtifactError> {
    let [vector] = vectors else {
        return Err(invalid(
            "challenge lifecycle vectors must contain exactly one outcome",
        ));
    };
    if vector.name != "issued-to-expired-at-boundary" {
        return Err(invalid("challenge lifecycle vector has an unknown name"));
    }
    parse_timestamp(&vector.observed_at, "observed_at")?;
    vector.issued_challenge.validate()?;
    vector.expected_terminalized_challenge.validate()?;

    let event = &vector.expected_lifecycle_event;
    parse_timestamp(&event.challenge_expires_at, "challenge_expires_at")?;
    parse_timestamp(&event.occurred_at, "occurred_at")?;
    if event.schema_version != OWNER_CONTROL_STATE_SCHEMA_VERSION
        || event.authorizes_execution
        || event.from_state != OwnerControlChallengeState::Issued
        || event.to_state != OwnerControlChallengeState::Expired
        || event.transition_reason != OwnerControlTransitionReason::Expired
    {
        return Err(invalid(
            "challenge lifecycle event has invalid terminal semantics",
        ));
    }
    if vector.issued_challenge.state != OwnerControlChallengeState::Issued
        || vector.expected_terminalized_challenge.state != OwnerControlChallengeState::Expired
        || vector.observed_at != vector.issued_challenge.expires_at
        || event.occurred_at != vector.observed_at
        || event.challenge_expires_at != vector.observed_at
    {
        return Err(invalid(
            "challenge lifecycle vector does not pin exact-boundary expiry",
        ));
    }

    let mut expected_terminalized = vector.issued_challenge.clone();
    expected_terminalized.state = OwnerControlChallengeState::Expired;
    expected_terminalized.terminal_event_id = Some(event.event_id.clone());
    if vector.expected_terminalized_challenge != expected_terminalized {
        return Err(invalid(
            "terminalized challenge must differ only by state and terminal event id",
        ));
    }
    if event.challenge_id != vector.issued_challenge.challenge_id
        || event.challenge_nonce != vector.issued_challenge.challenge_nonce
        || event.channel_session_id != vector.issued_challenge.channel_session_id
        || event.operation_id != vector.issued_challenge.operation_id
        || event.approval_request_sha256 != vector.issued_challenge.approval_request_sha256
        || event.binding_sha256 != vector.issued_challenge.binding_sha256
    {
        return Err(invalid(
            "challenge lifecycle event is not bound to its challenge",
        ));
    }
    Ok(())
}

impl OwnerControlChannelSessionRecord {
    fn validate(&self) -> Result<(), ArtifactError> {
        if self.schema_version != OWNER_CONTROL_STATE_SCHEMA_VERSION {
            return Err(invalid("channel session schema_version must be exactly 1"));
        }
        parse_timestamp(&self.enrolled_at, "enrolled_at")?;
        if let Some(revoked_at) = &self.revoked_at {
            parse_timestamp(revoked_at, "revoked_at")?;
        }
        match (self.status, self.revoked_at.is_some()) {
            (OwnerControlChannelSessionStatus::Enrolled, false)
            | (OwnerControlChannelSessionStatus::Revoked, true) => {}
            _ => return Err(invalid("channel session status and revoked_at disagree")),
        }

        let binding = parse_channel_binding(&self.binding_json, &self.binding_sha256)?;
        if binding.channel_session_id != self.channel_session_id
            || binding.owner_github_id != self.owner_github_id
        {
            return Err(invalid(
                "channel session does not match its canonical binding",
            ));
        }
        Ok(())
    }
}

impl OwnerControlChallengeRecord {
    fn validate(&self) -> Result<(), ArtifactError> {
        if self.schema_version != OWNER_CONTROL_STATE_SCHEMA_VERSION {
            return Err(invalid("challenge schema_version must be exactly 1"));
        }
        if self.attempt_count > OWNER_CONTROL_MAX_ATTEMPTS {
            return Err(invalid(
                "challenge attempt_count exceeds the published budget",
            ));
        }
        let issued_at = parse_timestamp(&self.issued_at, "issued_at")?;
        let expires_at = parse_timestamp(&self.expires_at, "expires_at")?;
        if expires_at <= issued_at {
            return Err(invalid("challenge expires_at must be later than issued_at"));
        }
        if let Some(consumed_at) = &self.consumed_at {
            parse_timestamp(consumed_at, "consumed_at")?;
        }
        match self.state {
            OwnerControlChallengeState::Issued
                if self.consumed_at.is_none() && self.terminal_event_id.is_none() => {}
            OwnerControlChallengeState::Consumed
                if self.consumed_at.is_some() && self.terminal_event_id.is_some() => {}
            OwnerControlChallengeState::Expired | OwnerControlChallengeState::Rejected
                if self.consumed_at.is_none() && self.terminal_event_id.is_some() => {}
            _ => return Err(invalid("challenge state and terminal markers disagree")),
        }

        let approval_request =
            parse_approval_request(&self.approval_request_json, &self.approval_request_sha256)?;
        let binding = parse_channel_binding(&self.binding_json, &self.binding_sha256)?;
        if approval_request.operation_id != self.operation_id
            || approval_request.owner_github_id != self.owner_github_id
            || approval_request.descriptor_id != self.descriptor_id
            || approval_request.nonce != self.challenge_nonce
            || approval_request.issued_at != self.issued_at
            || approval_request.expires_at != self.expires_at
            || binding.channel_session_id != self.channel_session_id
            || binding.owner_github_id != self.owner_github_id
        {
            return Err(invalid(
                "challenge does not match its canonical request and binding",
            ));
        }
        Ok(())
    }
}

fn validate_vector_record_presence(vector: &VerificationStateVector) -> Result<(), ArtifactError> {
    let expected_session_presence = vector.name != "unknown-channel-session";
    let expected_challenge_presence = !matches!(
        vector.name.as_str(),
        "unknown-channel-session" | "unknown-challenge"
    );
    if vector.channel_session.is_some() != expected_session_presence
        || vector.issued_challenge.is_some() != expected_challenge_presence
    {
        return Err(ArtifactError::Invalid(format!(
            "verification vector {} has inconsistent record presence",
            vector.name
        )));
    }

    if let Some(session) = &vector.channel_session {
        let expected_status = if vector.name == "channel-session-revoked" {
            OwnerControlChannelSessionStatus::Revoked
        } else {
            OwnerControlChannelSessionStatus::Enrolled
        };
        if session.status != expected_status {
            return Err(ArtifactError::Invalid(format!(
                "verification vector {} has inconsistent channel session state",
                vector.name
            )));
        }
    }
    if let Some(challenge) = &vector.issued_challenge {
        let expected_state = match vector.name.as_str() {
            "challenge-replayed" => OwnerControlChallengeState::Consumed,
            "attempt-budget-exhausted" => OwnerControlChallengeState::Rejected,
            _ => OwnerControlChallengeState::Issued,
        };
        if challenge.state != expected_state
            || (vector.name == "attempt-budget-exhausted"
                && challenge.attempt_count != OWNER_CONTROL_MAX_ATTEMPTS)
        {
            return Err(ArtifactError::Invalid(format!(
                "verification vector {} has inconsistent challenge state",
                vector.name
            )));
        }
    }
    Ok(())
}

fn validate_named_state_condition(
    vector: &VerificationStateVector,
    envelope: &OwnerControlConfirmationEnvelope,
) -> Result<(), ArtifactError> {
    let session_binding = vector
        .channel_session
        .as_ref()
        .map(|session| parse_channel_binding(&session.binding_json, &session.binding_sha256))
        .transpose()?;
    let challenge_request = vector
        .issued_challenge
        .as_ref()
        .map(|challenge| {
            parse_approval_request(
                &challenge.approval_request_json,
                &challenge.approval_request_sha256,
            )
        })
        .transpose()?;
    let challenge_binding = vector
        .issued_challenge
        .as_ref()
        .map(|challenge| parse_channel_binding(&challenge.binding_json, &challenge.binding_sha256))
        .transpose()?;
    let envelope_binding = &envelope.channel_binding;
    let envelope_request = &envelope.challenge_response.approval_request;

    match vector.name.as_str() {
        "unknown-channel-session" => {}
        "unknown-challenge" => {
            require_state_condition(
                session_binding.as_ref() == Some(envelope_binding),
                &vector.name,
            )?;
        }
        "challenge-channel-session-mismatch" => {
            let session_binding = session_binding
                .as_ref()
                .ok_or_else(|| missing_state_record(&vector.name))?;
            let mut expected_session_binding = envelope_binding.clone();
            expected_session_binding.channel_session_id =
                session_binding.channel_session_id.clone();
            require_state_condition(
                session_binding == &expected_session_binding
                    && challenge_binding.as_ref() == Some(envelope_binding)
                    && challenge_request.as_ref() == Some(envelope_request)
                    && expected_session_binding.channel_session_id
                        != envelope_binding.channel_session_id,
                &vector.name,
            )?;
        }
        "stored-binding-mismatch" => {
            let stored_binding = challenge_binding
                .as_ref()
                .ok_or_else(|| missing_state_record(&vector.name))?;
            let mut expected_envelope_binding = stored_binding.clone();
            expected_envelope_binding.session_expires_at =
                envelope_binding.session_expires_at.clone();
            require_state_condition(
                session_binding.as_ref() == Some(stored_binding)
                    && challenge_request.as_ref() == Some(envelope_request)
                    && &expected_envelope_binding == envelope_binding
                    && stored_binding != envelope_binding,
                &vector.name,
            )?;
        }
        "stored-approval-request-mismatch" => {
            let stored_request = challenge_request
                .as_ref()
                .ok_or_else(|| missing_state_record(&vector.name))?;
            let mut expected_envelope_request = stored_request.clone();
            expected_envelope_request.policy_revision = envelope_request.policy_revision;
            require_state_condition(
                session_binding.as_ref() == Some(envelope_binding)
                    && challenge_binding.as_ref() == Some(envelope_binding)
                    && &expected_envelope_request == envelope_request
                    && stored_request != envelope_request,
                &vector.name,
            )?;
        }
        _ => {
            require_state_condition(
                session_binding.as_ref() == Some(envelope_binding)
                    && challenge_binding.as_ref() == Some(envelope_binding)
                    && challenge_request.as_ref() == Some(envelope_request),
                &vector.name,
            )?;
        }
    }

    let observed_at = parse_timestamp(&vector.observed_at, "observed_at")?;
    match vector.name.as_str() {
        "channel-session-revoked" => {
            let revoked_at = vector
                .channel_session
                .as_ref()
                .and_then(|session| session.revoked_at.as_deref())
                .ok_or_else(|| missing_state_record(&vector.name))?;
            let revoked_at = parse_timestamp(revoked_at, "revoked_at")?;
            require_state_condition(revoked_at <= observed_at, &vector.name)?;
        }
        "channel-session-expired" => {
            let expires_at =
                parse_timestamp(&envelope_binding.session_expires_at, "session_expires_at")?;
            require_state_condition(observed_at > expires_at, &vector.name)?;
        }
        "challenge-expired" => {
            let challenge = vector
                .issued_challenge
                .as_ref()
                .ok_or_else(|| missing_state_record(&vector.name))?;
            let expires_at = parse_timestamp(&challenge.expires_at, "expires_at")?;
            require_state_condition(observed_at > expires_at, &vector.name)?;
        }
        "challenge-replayed" => {
            let consumed_at = vector
                .issued_challenge
                .as_ref()
                .and_then(|challenge| challenge.consumed_at.as_deref())
                .ok_or_else(|| missing_state_record(&vector.name))?;
            let consumed_at = parse_timestamp(consumed_at, "consumed_at")?;
            require_state_condition(consumed_at <= observed_at, &vector.name)?;
        }
        "verified" => {
            let challenge = vector
                .issued_challenge
                .as_ref()
                .ok_or_else(|| missing_state_record(&vector.name))?;
            let issued_at = parse_timestamp(&challenge.issued_at, "issued_at")?;
            let expires_at = parse_timestamp(&challenge.expires_at, "expires_at")?;
            require_state_condition(
                issued_at <= observed_at && observed_at <= expires_at,
                &vector.name,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn require_state_condition(condition: bool, vector_name: &str) -> Result<(), ArtifactError> {
    if !condition {
        return Err(ArtifactError::Invalid(format!(
            "verification vector {vector_name} does not demonstrate its named state condition"
        )));
    }
    Ok(())
}

fn missing_state_record(vector_name: &str) -> ArtifactError {
    ArtifactError::Invalid(format!(
        "verification vector {vector_name} is missing a required state record"
    ))
}

fn parse_approval_request(
    canonical_json: &str,
    expected_sha256: &str,
) -> Result<ApprovalRequest, ArtifactError> {
    let value: Value = serde_json::from_str(canonical_json)?;
    validate_canonical_payload(canonical_json, expected_sha256, &value)?;
    ApprovalRequest::from_value(value).map_err(ArtifactError::from)
}

fn parse_channel_binding(
    canonical_json: &str,
    expected_sha256: &str,
) -> Result<ChannelBindingRecord, ArtifactError> {
    let value: Value = serde_json::from_str(canonical_json)?;
    validate_canonical_payload(canonical_json, expected_sha256, &value)?;
    ChannelBindingRecord::from_value(value).map_err(ArtifactError::from)
}

fn validate_canonical_payload(
    published_json: &str,
    expected_sha256: &str,
    value: &Value,
) -> Result<(), ArtifactError> {
    if canonical_json_bytes(value)? != published_json.as_bytes()
        || canonical_json_sha256(value)? != expected_sha256
    {
        return Err(invalid("state record canonical payload or digest drifted"));
    }
    Ok(())
}

fn expected_rejection_reasons() -> BTreeSet<OwnerControlRejectionReason> {
    [
        OwnerControlRejectionReason::UnknownChannelSession,
        OwnerControlRejectionReason::UnknownChallenge,
        OwnerControlRejectionReason::ChannelSessionRevoked,
        OwnerControlRejectionReason::ChannelSessionExpired,
        OwnerControlRejectionReason::ChallengeChannelSessionMismatch,
        OwnerControlRejectionReason::ChallengeExpired,
        OwnerControlRejectionReason::ChallengeReplayed,
        OwnerControlRejectionReason::StoredBindingMismatch,
        OwnerControlRejectionReason::StoredApprovalRequestMismatch,
        OwnerControlRejectionReason::SignatureInvalid,
        OwnerControlRejectionReason::AttemptBudgetExhausted,
    ]
    .into_iter()
    .collect()
}

fn invalid(message: &str) -> ArtifactError {
    ArtifactError::Invalid(message.to_owned())
}
