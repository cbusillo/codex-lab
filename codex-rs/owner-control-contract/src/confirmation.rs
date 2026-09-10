use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::Signature;
use ed25519_dalek::Verifier;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use serde::Serialize;

use crate::ChallengeResponse;
use crate::ValidationError;
use crate::canonical_json_bytes;
use crate::canonical_json_sha256;
use crate::decode::deserialize_value;
use crate::model::OWNER_CONTROL_SCHEMA_VERSION;
use crate::model::parse_timestamp;
use crate::model::validate_identifier;

pub const OWNER_CONTROL_SIGNATURE_DOMAIN: &str = "launchplane-owner-control-confirmation-v1";
pub const OWNER_CONTROL_SIGNATURE_ALGORITHM: &str = "ed25519";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChannelBindingRecord {
    #[serde(default = "default_schema_version")]
    pub schema_version: u8,
    pub channel_session_id: String,
    pub owner_github_id: i64,
    #[serde(default = "default_signature_algorithm")]
    pub signature_algorithm: String,
    pub owner_public_key: String,
    pub session_issued_at: String,
    pub session_expires_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlSignaturePayload {
    #[serde(default = "default_schema_version")]
    pub schema_version: u8,
    #[serde(default = "default_signature_domain")]
    pub domain: String,
    pub challenge_response: ChallengeResponse,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlConfirmationEnvelope {
    #[serde(default = "default_schema_version")]
    pub schema_version: u8,
    pub channel_binding: ChannelBindingRecord,
    pub challenge_response: ChallengeResponse,
    #[serde(default = "default_signature_algorithm")]
    pub signature_algorithm: String,
    pub signature: String,
}

impl ChannelBindingRecord {
    pub fn from_value(value: serde_json::Value) -> Result<Self, ValidationError> {
        let binding: Self = deserialize_value(value)?;
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != OWNER_CONTROL_SCHEMA_VERSION {
            return Err(ValidationError::Field {
                field: "schema_version",
                rule: "exactly 1",
            });
        }
        validate_identifier(&self.channel_session_id, "channel_session_id")?;
        if !(1..=i64::MAX).contains(&self.owner_github_id) {
            return Err(ValidationError::Field {
                field: "owner_github_id",
                rule: "a positive signed 64-bit integer",
            });
        }
        validate_signature_algorithm(&self.signature_algorithm)?;
        validate_base64url_field(
            &self.owner_public_key,
            "owner_public_key",
            /*encoded_length*/ 43,
        )?;
        decode_canonical_base64url::<32>(&self.owner_public_key, "owner_public_key")?;
        let session_issued_at = parse_timestamp(&self.session_issued_at, "session_issued_at")?;
        let session_expires_at = parse_timestamp(&self.session_expires_at, "session_expires_at")?;
        if session_expires_at <= session_issued_at {
            return Err(ValidationError::Rule {
                rule: "session_expires_at must be later than session_issued_at",
            });
        }
        Ok(())
    }
}

impl OwnerControlSignaturePayload {
    pub fn from_value(value: serde_json::Value) -> Result<Self, ValidationError> {
        let payload: Self = deserialize_value(value)?;
        payload.validate()?;
        Ok(payload)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != OWNER_CONTROL_SCHEMA_VERSION {
            return Err(ValidationError::Field {
                field: "schema_version",
                rule: "exactly 1",
            });
        }
        if self.domain != OWNER_CONTROL_SIGNATURE_DOMAIN {
            return Err(ValidationError::Field {
                field: "domain",
                rule: "the owner-control confirmation v1 domain",
            });
        }
        self.challenge_response
            .validate()
            .map_err(|error| error.at_field("challenge_response"))?;
        Ok(())
    }
}

impl OwnerControlConfirmationEnvelope {
    pub fn from_value(value: serde_json::Value) -> Result<Self, ValidationError> {
        let envelope: Self = deserialize_value(value)?;
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != OWNER_CONTROL_SCHEMA_VERSION {
            return Err(ValidationError::Field {
                field: "schema_version",
                rule: "exactly 1",
            });
        }
        self.channel_binding
            .validate()
            .map_err(|error| error.at_field("channel_binding"))?;
        self.challenge_response
            .validate()
            .map_err(|error| error.at_field("challenge_response"))?;
        validate_signature_algorithm(&self.signature_algorithm)?;
        validate_base64url_field(&self.signature, "signature", /*encoded_length*/ 86)?;
        decode_canonical_base64url::<64>(&self.signature, "signature")?;
        if self.challenge_response.channel_binding_sha256
            != channel_binding_sha256(&self.channel_binding)?
        {
            return Err(ValidationError::Rule {
                rule: "challenge_response channel binding digest does not match the binding record",
            });
        }
        if self.challenge_response.approval_request.owner_github_id
            != self.channel_binding.owner_github_id
        {
            return Err(ValidationError::Rule {
                rule: "channel binding owner identity does not match the approval request owner",
            });
        }
        let session_issued_at =
            parse_timestamp(&self.channel_binding.session_issued_at, "session_issued_at")?;
        let session_expires_at = parse_timestamp(
            &self.channel_binding.session_expires_at,
            "session_expires_at",
        )?;
        let request_issued_at = parse_timestamp(
            &self.challenge_response.approval_request.issued_at,
            "issued_at",
        )?;
        let request_expires_at = parse_timestamp(
            &self.challenge_response.approval_request.expires_at,
            "expires_at",
        )?;
        let confirmed_at = parse_timestamp(&self.challenge_response.confirmed_at, "confirmed_at")?;
        if request_issued_at < session_issued_at || request_expires_at > session_expires_at {
            return Err(ValidationError::Rule {
                rule: "approval request bounds must be inside the channel session interval",
            });
        }
        if confirmed_at < session_issued_at || confirmed_at > session_expires_at {
            return Err(ValidationError::Rule {
                rule: "confirmation time must be inside the channel session interval",
            });
        }
        Ok(())
    }
}

pub fn channel_binding_sha256(binding: &ChannelBindingRecord) -> Result<String, ValidationError> {
    let value = serde_json::to_value(binding).map_err(|source| ValidationError::Json {
        location: Vec::new(),
        source,
    })?;
    Ok(canonical_json_sha256(&value)?)
}

pub fn signature_payload(response: &ChallengeResponse) -> OwnerControlSignaturePayload {
    OwnerControlSignaturePayload {
        schema_version: OWNER_CONTROL_SCHEMA_VERSION,
        domain: OWNER_CONTROL_SIGNATURE_DOMAIN.to_owned(),
        challenge_response: response.clone(),
    }
}

pub fn signature_payload_bytes(response: &ChallengeResponse) -> Result<Vec<u8>, ValidationError> {
    let value = serde_json::to_value(signature_payload(response)).map_err(|source| {
        ValidationError::Json {
            location: Vec::new(),
            source,
        }
    })?;
    Ok(canonical_json_bytes(&value)?)
}

/// Verifies only the envelope's internal Ed25519 signature proof.
///
/// This does not establish runtime authorization. A caller must separately match the exact
/// binding and challenge against server-enrolled session and issued challenge records.
pub fn verify_confirmation_signature_proof(envelope: &OwnerControlConfirmationEnvelope) -> bool {
    if envelope.validate().is_err() {
        return false;
    }
    let Ok(public_key_bytes) = decode_canonical_base64url::<32>(
        &envelope.channel_binding.owner_public_key,
        "owner_public_key",
    ) else {
        return false;
    };
    let Ok(signature_bytes) = decode_canonical_base64url::<64>(&envelope.signature, "signature")
    else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&public_key_bytes) else {
        return false;
    };
    let signature = Signature::from_bytes(&signature_bytes);
    let Ok(payload) = signature_payload_bytes(&envelope.challenge_response) else {
        return false;
    };
    verifying_key.verify(&payload, &signature).is_ok()
}

fn validate_signature_algorithm(value: &str) -> Result<(), ValidationError> {
    if value != OWNER_CONTROL_SIGNATURE_ALGORITHM {
        return Err(ValidationError::Field {
            field: "signature_algorithm",
            rule: "exactly ed25519",
        });
    }
    Ok(())
}

fn validate_base64url_field(
    value: &str,
    field: &'static str,
    encoded_length: usize,
) -> Result<(), ValidationError> {
    if value.len() != encoded_length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ValidationError::Field {
            field,
            rule: "canonical unpadded base64url with the required encoded length",
        });
    }
    Ok(())
}

fn decode_canonical_base64url<const LENGTH: usize>(
    value: &str,
    field: &'static str,
) -> Result<[u8; LENGTH], ValidationError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| base64url_rule(field))?;
    let bytes: [u8; LENGTH] = decoded.try_into().map_err(|_| base64url_rule(field))?;
    if URL_SAFE_NO_PAD.encode(bytes) != value {
        return Err(base64url_rule(field));
    }
    Ok(bytes)
}

fn base64url_rule(field: &'static str) -> ValidationError {
    match field {
        "owner_public_key" => ValidationError::Rule {
            rule: "owner_public_key must use canonical unpadded base64url",
        },
        "signature" => ValidationError::Rule {
            rule: "signature must use canonical unpadded base64url",
        },
        _ => ValidationError::Rule {
            rule: "value must use canonical unpadded base64url",
        },
    }
}

fn default_schema_version() -> u8 {
    OWNER_CONTROL_SCHEMA_VERSION
}

fn default_signature_algorithm() -> String {
    OWNER_CONTROL_SIGNATURE_ALGORITHM.to_owned()
}

fn default_signature_domain() -> String {
    OWNER_CONTROL_SIGNATURE_DOMAIN.to_owned()
}
