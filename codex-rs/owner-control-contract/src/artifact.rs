use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use thiserror::Error;

use crate::ApprovalRequest;
use crate::CanonicalJsonError;
use crate::ChallengeResponse;
use crate::DescriptorId;
use crate::ErrorLocation;
use crate::OWNER_CONTROL_SCHEMA_VERSION;
use crate::ServerReviewPayload;
use crate::approval_request_digest;
use crate::canonical_json_bytes;
use crate::canonical_json_sha256;
use crate::model::ValidationError;

pub const EMBEDDED_CONTRACT_JSON: &str = include_str!("../contracts/owner-control-contract.json");
pub const EMBEDDED_CONTRACT_SHA256: &str =
    "b4ce407a5cfdfb8336924db5a0ab4b887b701ebb76fcb36d8577250d0899e064";

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

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContractArtifact {
    pub canonical_json: CanonicalJsonSpec,
    pub canonicalization_vectors: Vec<CanonicalizationVector>,
    pub golden_vectors: Vec<GoldenVector>,
    pub negative_vectors: Vec<NegativeVector>,
    pub schema_version: u8,
    pub schemas: BTreeMap<String, Value>,
}

impl ContractArtifact {
    pub fn validate(&self) -> Result<(), ArtifactError> {
        if self.schema_version != OWNER_CONTROL_SCHEMA_VERSION {
            return Err(ArtifactError::Invalid(
                "schema_version must be exactly 1".to_owned(),
            ));
        }
        self.canonical_json.validate()?;
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
        let expected_schemas = [
            "approval_request",
            "challenge_response",
            "server_review_payload",
        ];
        if self.schemas.keys().map(String::as_str).collect::<Vec<_>>() != expected_schemas {
            return Err(ArtifactError::Invalid(
                "schemas must contain the three published model schemas".to_owned(),
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
