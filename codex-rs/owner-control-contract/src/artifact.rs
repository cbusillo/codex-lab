mod compatibility;
mod state_vectors;

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use thiserror::Error;

use crate::ApprovalRequest;
use crate::CanonicalJsonError;
use crate::ChallengeResponse;
use crate::ChannelBindingRecord;
use crate::DescriptorId;
use crate::ErrorLocation;
use crate::OwnerControlConfirmationEnvelope;
use crate::OwnerControlEnrollmentContext;
use crate::OwnerControlEnrollmentProvenance;
use crate::OwnerControlHostPrincipalClaim;
use crate::OwnerControlProvenanceResult;
use crate::OwnerControlProvenanceTier;
use crate::OwnerControlServerObservedCorroboration;
use crate::OwnerControlSignaturePayload;
use crate::ServerReviewPayload;
use crate::approval_request_digest;
use crate::canonical_json_bytes;
use crate::canonical_json_sha256;
use crate::channel_binding_sha256;
use crate::model::ValidationError;
use crate::owner_control_host_principal_claim_sha256;
use crate::provenance::is_published_synthetic_public_key_sha256;
use crate::signature_payload;
use crate::verify_confirmation_signature_proof;

pub use compatibility::CompatibilityDeclaration;
pub use state_vectors::ChallengeLifecycleEvent;
pub use state_vectors::ChallengeLifecycleVector;
pub use state_vectors::OwnerControlAuthorityState;
pub use state_vectors::OwnerControlChallengeRecord;
pub use state_vectors::OwnerControlChallengeState;
pub use state_vectors::OwnerControlChannelSessionRecord;
pub use state_vectors::OwnerControlChannelSessionStatus;
pub use state_vectors::OwnerControlRejectionReason;
pub use state_vectors::OwnerControlTransitionReason;
pub use state_vectors::OwnerControlVerificationStatus;
pub use state_vectors::OwnerControlVerifierMode;
pub use state_vectors::VerificationStateExpectation;
pub use state_vectors::VerificationStateVector;

pub const EMBEDDED_CONTRACT_JSON: &str = include_str!("../contracts/owner-control-contract.json");
pub const EMBEDDED_CONTRACT_SHA256: &str =
    "cf2815b65bafb7e25b00647dbdfd464577cb0a6e8a861ae3e1e019840865804e";
pub const OWNER_CONTROL_CONTRACT_SCHEMA_VERSION: u8 = 5;
const PREVIOUS_OWNER_CONTROL_CONTRACT_SCHEMA_VERSION: u8 = 4;
const PRESERVED_V2_SIGNATURE_CONTRACT_SCHEMA_VERSION: u8 = 2;
const OWNER_CONTROL_STATE_SCHEMA_VERSION: u8 = 1;
const OWNER_CONTROL_MAX_ATTEMPTS: u8 = 8;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error(transparent)]
    CanonicalJson(#[from] CanonicalJsonError),
    #[error("invalid owner-control contract artifact: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalJsonSpec {
    pub encoding: String,
    pub ensure_ascii: bool,
    pub integer_max: i64,
    pub integer_min: i64,
    pub non_finite_numbers: String,
    pub number_domain: String,
    pub object_key_order: String,
    pub object_keys: String,
    pub separators: [String; 2],
    pub trailing_newline: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalizationVector {
    pub canonical_json: String,
    pub name: String,
    pub payload: Value,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GoldenPayload<T> {
    pub canonical_json: String,
    pub payload: T,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GoldenVector {
    pub approval_request: GoldenPayload<ApprovalRequest>,
    pub challenge_response: GoldenPayload<ChallengeResponse>,
    pub descriptor_id: DescriptorId,
    pub descriptor_version: u8,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignatureDeclaration {
    pub algorithm: String,
    pub contract_schema_version: u8,
    pub domain: String,
    pub legacy_golden_channel_binding: String,
    pub payload: String,
    pub payload_encoding: String,
    pub public_key_bytes: u8,
    pub public_key_encoding: String,
    pub signature_bytes: u8,
    pub signature_encoding: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    Valid,
    Invalid,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationGoldenVector {
    pub channel_binding: GoldenPayload<ChannelBindingRecord>,
    pub challenge_response: GoldenPayload<ChallengeResponse>,
    pub signature_payload: GoldenPayload<OwnerControlSignaturePayload>,
    pub confirmation_envelope: GoldenPayload<OwnerControlConfirmationEnvelope>,
    pub descriptor_id: DescriptorId,
    pub descriptor_version: u8,
    pub verification: VerificationOutcome,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegativeModel {
    ApprovalRequest,
    ChallengeResponse,
    ServerReviewPayload,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NegativeVector {
    pub error_location: Vec<ErrorLocation>,
    pub model: NegativeModel,
    pub payload: Value,
    pub rule: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegativeConfirmationModel {
    OwnerControlConfirmationEnvelope,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NegativeConfirmationVector {
    pub error_location: Vec<ErrorLocation>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub error_message_contains: Option<String>,
    pub model: NegativeConfirmationModel,
    pub payload: Value,
    pub rule: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub verification: Option<VerificationOutcome>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceDeclaration {
    pub authority_state: OwnerControlAuthorityState,
    pub authorizes_execution: bool,
    pub claim_source: String,
    pub enrollment_context: OwnerControlEnrollmentContext,
    pub provenance_schema_version: u8,
    pub provenance_tier: OwnerControlProvenanceTier,
    pub runtime_synthetic_key_policy: String,
    pub server_observed_corroboration: OwnerControlServerObservedCorroboration,
    pub trust_derivation: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceVector {
    pub claim: GoldenPayload<OwnerControlHostPrincipalClaim>,
    pub enrollment_provenance: GoldenPayload<OwnerControlEnrollmentProvenance>,
    pub result: OwnerControlProvenanceResult,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegativeProvenanceModel {
    OwnerControlEnrollmentProvenance,
    OwnerControlHostPrincipalClaim,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegativeProvenanceOperation {
    EnrollChannelSession,
    IssueChallenge,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegativeProvenanceResult {
    Reject,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NegativeProvenanceVector {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub channel_session_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub error_location: Option<Vec<ErrorLocation>>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub model: Option<NegativeProvenanceModel>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub operation: Option<NegativeProvenanceOperation>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner_public_key_sha256: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub payload: Option<Value>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<NegativeProvenanceResult>,
    pub rule: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub runtime_guard_matches: Option<bool>,
}

fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("field must be omitted instead of null"))
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContractArtifact {
    pub canonical_json: CanonicalJsonSpec,
    pub canonicalization_vectors: Vec<CanonicalizationVector>,
    pub challenge_lifecycle_vectors: Vec<ChallengeLifecycleVector>,
    pub compatibility: CompatibilityDeclaration,
    pub confirmation_golden_vectors: Vec<ConfirmationGoldenVector>,
    pub golden_vectors: Vec<GoldenVector>,
    pub negative_confirmation_vectors: Vec<NegativeConfirmationVector>,
    pub negative_provenance_vectors: Vec<NegativeProvenanceVector>,
    pub negative_vectors: Vec<NegativeVector>,
    pub provenance_declaration: ProvenanceDeclaration,
    pub provenance_schemas: BTreeMap<String, Value>,
    pub provenance_vectors: Vec<ProvenanceVector>,
    pub schema_version: u8,
    pub schemas: BTreeMap<String, Value>,
    pub signature_declaration: SignatureDeclaration,
    pub verification_state_vectors: Vec<VerificationStateVector>,
}

impl ContractArtifact {
    pub fn validate(&self) -> Result<(), ArtifactError> {
        if self.schema_version != OWNER_CONTROL_CONTRACT_SCHEMA_VERSION {
            return Err(ArtifactError::Invalid(
                "contract schema_version must be exactly 5".to_owned(),
            ));
        }
        self.compatibility.validate(self)?;
        self.canonical_json.validate()?;
        self.signature_declaration.validate()?;
        for vector in &self.canonicalization_vectors {
            let canonical = canonical_json_bytes(&vector.payload)?;
            if canonical != vector.canonical_json.as_bytes() {
                return Err(ArtifactError::Invalid(format!(
                    "canonicalization vector {} has incorrect canonical_json",
                    vector.name
                )));
            }
            if canonical_json_sha256(&vector.payload)? != vector.sha256 {
                return Err(ArtifactError::Invalid(format!(
                    "canonicalization vector {} has incorrect sha256",
                    vector.name
                )));
            }
        }
        for vector in &self.golden_vectors {
            vector.validate()?;
        }
        for vector in &self.confirmation_golden_vectors {
            vector.validate()?;
        }
        for vector in &self.negative_vectors {
            match parse_negative_payload(vector) {
                Ok(()) => {
                    return Err(ArtifactError::Invalid(format!(
                        "negative vector {} was accepted",
                        vector.rule
                    )));
                }
                Err(ArtifactError::Validation(error))
                    if error.location() == vector.error_location => {}
                Err(ArtifactError::Validation(error)) => {
                    return Err(ArtifactError::Invalid(format!(
                        "negative vector {} failed at {:?}, expected {:?}",
                        vector.rule,
                        error.location(),
                        vector.error_location
                    )));
                }
                Err(error) => return Err(error),
            }
        }
        for vector in &self.negative_confirmation_vectors {
            validate_negative_confirmation_vector(vector)?;
        }
        self.provenance_declaration.validate()?;
        validate_provenance_vectors(&self.provenance_vectors)?;
        validate_negative_provenance_vectors(&self.negative_provenance_vectors)?;
        let expected_schemas = [
            "approval_request",
            "challenge_response",
            "channel_binding_record",
            "owner_control_confirmation_envelope",
            "owner_control_signature_payload",
            "server_review_payload",
        ];
        if self.schemas.keys().map(String::as_str).collect::<Vec<_>>() != expected_schemas {
            return Err(ArtifactError::Invalid(
                "schemas must contain the six published model schemas".to_owned(),
            ));
        }
        let expected_provenance_schemas = [
            "owner_control_enrollment_provenance",
            "owner_control_host_principal_claim",
        ];
        if self
            .provenance_schemas
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            != expected_provenance_schemas
        {
            return Err(ArtifactError::Invalid(
                "provenance_schemas must contain the two published provenance models".to_owned(),
            ));
        }
        let expected_provenance_schema_sha256 = [
            (
                "owner_control_enrollment_provenance",
                "b52b43f6b17c750543d238ffc0399a4892ada0d6a6f2b611973b2c8577268033",
            ),
            (
                "owner_control_host_principal_claim",
                "f384cb1095888940d1196a105a78f889ab7e38496d77c4ec9489a7d07cce1327",
            ),
        ];
        for (schema, expected_sha256) in expected_provenance_schema_sha256 {
            let actual_sha256 = canonical_json_sha256(&self.provenance_schemas[schema])?;
            if actual_sha256 != expected_sha256 {
                return Err(ArtifactError::Invalid(format!(
                    "published provenance schema {schema} has SHA-256 {actual_sha256}, expected {expected_sha256}"
                )));
            }
        }
        state_vectors::validate_verification_state_vectors(&self.verification_state_vectors)?;
        state_vectors::validate_challenge_lifecycle_vectors(&self.challenge_lifecycle_vectors)?;
        Ok(())
    }
}

impl ProvenanceDeclaration {
    fn validate(&self) -> Result<(), ArtifactError> {
        if self.authority_state != OwnerControlAuthorityState::Inert
            || self.authorizes_execution
            || self.claim_source != "caller-declared"
            || self.enrollment_context != OwnerControlEnrollmentContext::PostgresRecordStore
            || self.provenance_schema_version != 1
            || self.provenance_tier != OwnerControlProvenanceTier::SelfAsserted
            || self.runtime_synthetic_key_policy != "reject-published-conformance-keys"
            || self.server_observed_corroboration != OwnerControlServerObservedCorroboration::None
            || self.trust_derivation != "corroboration-only"
        {
            return Err(ArtifactError::Invalid(
                "provenance_declaration does not match the inert v5 contract".to_owned(),
            ));
        }
        Ok(())
    }
}

fn validate_provenance_vectors(vectors: &[ProvenanceVector]) -> Result<(), ArtifactError> {
    use crate::OwnerControlGestureSourceClaim;
    use crate::OwnerControlKeyCustodyClaim;
    use crate::OwnerControlPrincipalSeparationClaim;

    if vectors.len() != 18 {
        return Err(ArtifactError::Invalid(
            "provenance_vectors must contain all eighteen claim combinations".to_owned(),
        ));
    }
    let mut combinations = std::collections::BTreeSet::new();
    for vector in vectors {
        vector.claim.payload.validate()?;
        vector.enrollment_provenance.payload.validate()?;
        validate_golden_payload(&vector.claim)?;
        validate_golden_payload(&vector.enrollment_provenance)?;
        if vector
            .enrollment_provenance
            .payload
            .host_principal_claim()?
            != vector.claim.payload
        {
            return Err(ArtifactError::Invalid(
                "provenance vector enrollment does not bind its exact claim".to_owned(),
            ));
        }
        if vector
            .enrollment_provenance
            .payload
            .host_principal_claim_sha256
            != owner_control_host_principal_claim_sha256(&vector.claim.payload)?
        {
            return Err(ArtifactError::Invalid(
                "provenance vector claim digest does not match its claim".to_owned(),
            ));
        }
        let expected = OwnerControlProvenanceResult {
            authority_state: OwnerControlAuthorityState::Inert,
            authorizes_execution: false,
            provenance_tier: OwnerControlProvenanceTier::SelfAsserted,
            server_observed_corroboration: OwnerControlServerObservedCorroboration::None,
        };
        if vector.result != expected {
            return Err(ArtifactError::Invalid(
                "provenance vector result must remain self-asserted and inert".to_owned(),
            ));
        }
        combinations.insert((
            vector.claim.payload.principal_separation,
            vector.claim.payload.key_custody,
            vector.claim.payload.gesture_source,
        ));
    }
    let expected_combinations = [
        OwnerControlPrincipalSeparationClaim::NotClaimed,
        OwnerControlPrincipalSeparationClaim::SharedRuntime,
        OwnerControlPrincipalSeparationClaim::SeparateOsPrincipal,
    ]
    .into_iter()
    .flat_map(|principal_separation| {
        [
            OwnerControlKeyCustodyClaim::NotClaimed,
            OwnerControlKeyCustodyClaim::SoftwareBacked,
            OwnerControlKeyCustodyClaim::HardwareBacked,
        ]
        .into_iter()
        .flat_map(move |key_custody| {
            [
                OwnerControlGestureSourceClaim::NotClaimed,
                OwnerControlGestureSourceClaim::LocalInteractive,
            ]
            .into_iter()
            .map(move |gesture_source| (principal_separation, key_custody, gesture_source))
        })
    })
    .collect::<std::collections::BTreeSet<_>>();
    if combinations != expected_combinations {
        return Err(ArtifactError::Invalid(
            "provenance_vectors are not exhaustive".to_owned(),
        ));
    }
    Ok(())
}

fn validate_negative_provenance_vectors(
    vectors: &[NegativeProvenanceVector],
) -> Result<(), ArtifactError> {
    if vectors.len() != 8 {
        return Err(ArtifactError::Invalid(
            "negative_provenance_vectors must contain eight published cases".to_owned(),
        ));
    }
    let mut structural_count = 0;
    let mut missing_provenance_count = 0;
    let mut synthetic_key_digests = std::collections::BTreeSet::new();
    for vector in vectors {
        if let (Some(model), Some(payload), Some(error_location)) =
            (vector.model, &vector.payload, &vector.error_location)
        {
            if vector.channel_session_id.is_some()
                || vector.operation.is_some()
                || vector.owner_public_key_sha256.is_some()
                || vector.result.is_some()
                || vector.runtime_guard_matches.is_some()
            {
                return Err(ArtifactError::Invalid(format!(
                    "negative provenance vector {} mixes structural and runtime fields",
                    vector.rule
                )));
            }
            structural_count += 1;
            let result = match model {
                NegativeProvenanceModel::OwnerControlEnrollmentProvenance => {
                    OwnerControlEnrollmentProvenance::from_value(payload.clone()).map(|_| ())
                }
                NegativeProvenanceModel::OwnerControlHostPrincipalClaim => {
                    OwnerControlHostPrincipalClaim::from_value(payload.clone()).map(|_| ())
                }
            };
            match result {
                Ok(()) => {
                    return Err(ArtifactError::Invalid(format!(
                        "negative provenance vector {} was accepted",
                        vector.rule
                    )));
                }
                Err(error) if error.location() == *error_location => {}
                Err(error) => {
                    return Err(ArtifactError::Invalid(format!(
                        "negative provenance vector {} failed at {:?}, expected {:?}",
                        vector.rule,
                        error.location(),
                        error_location
                    )));
                }
            }
            continue;
        }
        if vector.operation == Some(NegativeProvenanceOperation::IssueChallenge)
            && vector.result == Some(NegativeProvenanceResult::Reject)
            && vector.rule == "missing-enrollment-provenance-is-rejected"
            && vector.channel_session_id.is_some()
            && vector.model.is_none()
            && vector.payload.is_none()
            && vector.error_location.is_none()
            && vector.owner_public_key_sha256.is_none()
            && vector.runtime_guard_matches.is_none()
        {
            missing_provenance_count += 1;
            continue;
        }
        if let Some(owner_public_key_sha256) = vector.owner_public_key_sha256.as_deref()
            && vector.operation == Some(NegativeProvenanceOperation::EnrollChannelSession)
            && vector.result == Some(NegativeProvenanceResult::Reject)
            && vector.rule == "published-synthetic-conformance-key-is-rejected"
            && vector.runtime_guard_matches == Some(true)
            && vector.channel_session_id.is_none()
            && vector.error_location.is_none()
            && vector.model.is_none()
            && vector.payload.is_none()
            && is_published_synthetic_public_key_sha256(owner_public_key_sha256)
        {
            synthetic_key_digests.insert(owner_public_key_sha256);
            continue;
        }
        return Err(ArtifactError::Invalid(format!(
            "negative provenance vector {} has an unsupported shape",
            vector.rule
        )));
    }
    if structural_count != 4 || missing_provenance_count != 1 || synthetic_key_digests.len() != 3 {
        return Err(ArtifactError::Invalid(
            "negative_provenance_vectors do not cover the published guards".to_owned(),
        ));
    }
    Ok(())
}

impl SignatureDeclaration {
    fn validate(&self) -> Result<(), ArtifactError> {
        if self.algorithm != "ed25519"
            || self.contract_schema_version != PRESERVED_V2_SIGNATURE_CONTRACT_SCHEMA_VERSION
            || self.domain != "launchplane-owner-control-confirmation-v1"
            || self.legacy_golden_channel_binding
                != "synthetic-placeholder-not-channel-binding-record"
            || self.payload != "OwnerControlSignaturePayload"
            || self.payload_encoding != "canonical-json-utf8"
            || self.public_key_bytes != 32
            || self.public_key_encoding != "base64url-unpadded"
            || self.signature_bytes != 64
            || self.signature_encoding != "base64url-unpadded"
        {
            return Err(ArtifactError::Invalid(
                "signature_declaration does not match the preserved v2 section".to_owned(),
            ));
        }
        Ok(())
    }
}

impl CanonicalJsonSpec {
    fn validate(&self) -> Result<(), ArtifactError> {
        if self.encoding != "utf-8"
            || !self.ensure_ascii
            || self.integer_max != i64::MAX
            || self.integer_min != i64::MIN
            || self.non_finite_numbers != "rejected"
            || self.number_domain != "signed-64-bit-integers-only"
            || self.object_key_order != "unicode-code-point"
            || self.object_keys != "strings-only"
            || self.separators != [",", ":"]
            || self.trailing_newline
        {
            return Err(ArtifactError::Invalid(
                "canonical_json does not describe the supported serializer".to_owned(),
            ));
        }
        Ok(())
    }
}

impl GoldenVector {
    fn validate(&self) -> Result<(), ArtifactError> {
        if self.descriptor_version != 1
            || self.descriptor_id != self.approval_request.payload.descriptor_id
        {
            return Err(ArtifactError::Invalid(
                "golden vector descriptor metadata does not match its approval request".to_owned(),
            ));
        }
        self.approval_request.payload.validate()?;
        self.challenge_response.payload.validate()?;
        if self.challenge_response.payload.approval_request != self.approval_request.payload {
            return Err(ArtifactError::Invalid(
                "golden challenge response is not bound to its approval request".to_owned(),
            ));
        }
        validate_golden_payload(&self.approval_request)?;
        validate_golden_payload(&self.challenge_response)?;
        if self.challenge_response.payload.approval_request_digest
            != approval_request_digest(&self.approval_request.payload)?
        {
            return Err(ArtifactError::Invalid(
                "golden challenge response has incorrect approval request digest".to_owned(),
            ));
        }
        Ok(())
    }
}

impl ConfirmationGoldenVector {
    fn validate(&self) -> Result<(), ArtifactError> {
        if self.descriptor_version != 1
            || self.descriptor_id
                != self
                    .challenge_response
                    .payload
                    .approval_request
                    .descriptor_id
        {
            return Err(ArtifactError::Invalid(
                "confirmation vector descriptor metadata does not match its approval request"
                    .to_owned(),
            ));
        }
        self.channel_binding.payload.validate()?;
        self.challenge_response.payload.validate()?;
        self.signature_payload.payload.validate()?;
        self.confirmation_envelope.payload.validate()?;
        if self.signature_payload.payload.challenge_response != self.challenge_response.payload {
            return Err(ArtifactError::Invalid(
                "confirmation signature payload is not bound to its challenge response".to_owned(),
            ));
        }
        if self.confirmation_envelope.payload.channel_binding != self.channel_binding.payload
            || self.confirmation_envelope.payload.challenge_response
                != self.challenge_response.payload
        {
            return Err(ArtifactError::Invalid(
                "confirmation envelope does not contain the published binding and response"
                    .to_owned(),
            ));
        }
        validate_golden_payload(&self.channel_binding)?;
        validate_golden_payload(&self.challenge_response)?;
        validate_golden_payload(&self.signature_payload)?;
        validate_golden_payload(&self.confirmation_envelope)?;
        if channel_binding_sha256(&self.channel_binding.payload)? != self.channel_binding.sha256 {
            return Err(ArtifactError::Invalid(
                "confirmation channel binding digest is incorrect".to_owned(),
            ));
        }
        if signature_payload(&self.challenge_response.payload) != self.signature_payload.payload {
            return Err(ArtifactError::Invalid(
                "confirmation signature payload does not use the published domain".to_owned(),
            ));
        }
        if self.verification != VerificationOutcome::Valid
            || !verify_confirmation_signature_proof(&self.confirmation_envelope.payload)
        {
            return Err(ArtifactError::Invalid(
                "confirmation golden signature proof is not valid".to_owned(),
            ));
        }
        Ok(())
    }
}

fn validate_negative_confirmation_vector(
    vector: &NegativeConfirmationVector,
) -> Result<(), ArtifactError> {
    match vector.verification {
        None => match parse_negative_confirmation_payload(vector) {
            Ok(_) => Err(ArtifactError::Invalid(format!(
                "negative confirmation vector {} was accepted",
                vector.rule
            ))),
            Err(ArtifactError::Validation(error))
                if error.location() == vector.error_location
                    && vector
                        .error_message_contains
                        .as_ref()
                        .is_none_or(|message| error.to_string().contains(message)) =>
            {
                Ok(())
            }
            Err(ArtifactError::Validation(error)) => Err(ArtifactError::Invalid(format!(
                "negative confirmation vector {} failed as {} at {:?}, expected {:?}",
                vector.rule,
                error,
                error.location(),
                vector.error_location
            ))),
            Err(error) => Err(error),
        },
        Some(VerificationOutcome::Invalid) => {
            let envelope = parse_negative_confirmation_payload(vector)?;
            if verify_confirmation_signature_proof(&envelope) {
                return Err(ArtifactError::Invalid(format!(
                    "negative confirmation vector {} had a valid signature proof",
                    vector.rule
                )));
            }
            Ok(())
        }
        Some(VerificationOutcome::Valid) => Err(ArtifactError::Invalid(format!(
            "negative confirmation vector {} cannot expect valid verification",
            vector.rule
        ))),
    }
}

fn validate_golden_payload<T: serde::Serialize>(
    payload: &GoldenPayload<T>,
) -> Result<(), ArtifactError> {
    let value = serde_json::to_value(&payload.payload)?;
    if canonical_json_bytes(&value)? != payload.canonical_json.as_bytes()
        || canonical_json_sha256(&value)? != payload.sha256
    {
        return Err(ArtifactError::Invalid(
            "golden payload canonicalization does not match its digest".to_owned(),
        ));
    }
    Ok(())
}

pub fn load_embedded_artifact() -> Result<ContractArtifact, ArtifactError> {
    let actual_sha256 = format!("{:x}", Sha256::digest(EMBEDDED_CONTRACT_JSON.as_bytes()));
    if actual_sha256 != EMBEDDED_CONTRACT_SHA256 {
        return Err(ArtifactError::Invalid(format!(
            "embedded artifact SHA-256 is {actual_sha256}, expected {EMBEDDED_CONTRACT_SHA256}"
        )));
    }
    let raw: Value = serde_json::from_str(EMBEDDED_CONTRACT_JSON)?;
    let artifact: ContractArtifact = serde_json::from_value(raw.clone())?;
    if serde_json::to_value(&artifact)? != raw {
        return Err(ArtifactError::Invalid(
            "typed artifact does not preserve every published field".to_owned(),
        ));
    }
    artifact.validate()?;
    Ok(artifact)
}

pub fn parse_negative_payload(vector: &NegativeVector) -> Result<(), ArtifactError> {
    match vector.model {
        NegativeModel::ApprovalRequest => ApprovalRequest::from_value(vector.payload.clone())
            .map(|_| ())
            .map_err(ArtifactError::from),
        NegativeModel::ChallengeResponse => ChallengeResponse::from_value(vector.payload.clone())
            .map(|_| ())
            .map_err(ArtifactError::from),
        NegativeModel::ServerReviewPayload => {
            ServerReviewPayload::from_value(vector.payload.clone())
                .map(|_| ())
                .map_err(ArtifactError::from)
        }
    }
}

pub fn parse_negative_confirmation_payload(
    vector: &NegativeConfirmationVector,
) -> Result<OwnerControlConfirmationEnvelope, ArtifactError> {
    match vector.model {
        NegativeConfirmationModel::OwnerControlConfirmationEnvelope => {
            OwnerControlConfirmationEnvelope::from_value(vector.payload.clone())
                .map_err(ArtifactError::from)
        }
    }
}
