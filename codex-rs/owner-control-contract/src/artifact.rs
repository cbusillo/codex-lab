use std::collections::BTreeMap;

use serde::Deserialize;
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
use crate::OwnerControlSignaturePayload;
use crate::ServerReviewPayload;
use crate::approval_request_digest;
use crate::canonical_json_bytes;
use crate::canonical_json_sha256;
use crate::channel_binding_sha256;
use crate::model::ValidationError;
use crate::signature_payload;
use crate::verify_confirmation_signature_proof;

pub const EMBEDDED_CONTRACT_JSON: &str = include_str!("../contracts/owner-control-contract.json");
pub const EMBEDDED_CONTRACT_SHA256: &str =
    "342a07917bdfc1a0f4ee43e6ec2b55adebf301b2abfcdab3aa979ce38cf92cc5";
pub const OWNER_CONTROL_CONTRACT_SCHEMA_VERSION: u8 = 2;

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

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalizationVector {
    pub canonical_json: String,
    pub name: String,
    pub payload: Value,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GoldenPayload<T> {
    pub canonical_json: String,
    pub payload: T,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GoldenVector {
    pub approval_request: GoldenPayload<ApprovalRequest>,
    pub challenge_response: GoldenPayload<ChallengeResponse>,
    pub descriptor_id: DescriptorId,
    pub descriptor_version: u8,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    Valid,
    Invalid,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegativeModel {
    ApprovalRequest,
    ChallengeResponse,
    ServerReviewPayload,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NegativeVector {
    pub error_location: Vec<ErrorLocation>,
    pub model: NegativeModel,
    pub payload: Value,
    pub rule: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NegativeConfirmationModel {
    OwnerControlConfirmationEnvelope,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NegativeConfirmationVector {
    pub error_location: Vec<ErrorLocation>,
    pub error_message_contains: Option<String>,
    pub model: NegativeConfirmationModel,
    pub payload: Value,
    pub rule: String,
    pub verification: Option<VerificationOutcome>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContractArtifact {
    pub canonical_json: CanonicalJsonSpec,
    pub canonicalization_vectors: Vec<CanonicalizationVector>,
    pub confirmation_golden_vectors: Vec<ConfirmationGoldenVector>,
    pub golden_vectors: Vec<GoldenVector>,
    pub negative_confirmation_vectors: Vec<NegativeConfirmationVector>,
    pub negative_vectors: Vec<NegativeVector>,
    pub schema_version: u8,
    pub schemas: BTreeMap<String, Value>,
    pub signature_declaration: SignatureDeclaration,
}

impl ContractArtifact {
    pub fn validate(&self) -> Result<(), ArtifactError> {
        if self.schema_version != OWNER_CONTROL_CONTRACT_SCHEMA_VERSION {
            return Err(ArtifactError::Invalid(
                "contract schema_version must be exactly 2".to_owned(),
            ));
        }
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
        Ok(())
    }
}

impl SignatureDeclaration {
    fn validate(&self) -> Result<(), ArtifactError> {
        if self.algorithm != "ed25519"
            || self.contract_schema_version != OWNER_CONTROL_CONTRACT_SCHEMA_VERSION
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
                "signature_declaration does not match the published v2 contract".to_owned(),
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
    let artifact: ContractArtifact = serde_json::from_str(EMBEDDED_CONTRACT_JSON)?;
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
