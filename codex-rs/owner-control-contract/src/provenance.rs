use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use crate::ChannelBindingRecord;
use crate::ErrorLocation;
use crate::OwnerControlAuthorityState;
use crate::ValidationError;
use crate::canonical_json_bytes;
use crate::canonical_json_sha256;
use crate::channel_binding_sha256;
use crate::decode::deserialize_value;
use crate::model::parse_timestamp;

pub const OWNER_CONTROL_ENROLLMENT_PROVENANCE_SCHEMA_VERSION: u8 = 1;

const PUBLISHED_SYNTHETIC_PUBLIC_KEY_SHA256: [&str; 3] = [
    "141ddf2e77d4f690748cf74ecd390d44687d477b31b8931fa37abd02c35dbaba",
    "56475aa75463474c0285df5dbf2bcab73da651358839e9b77481b2eab107708c",
    "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925",
];

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlPrincipalSeparationClaim {
    NotClaimed,
    SharedRuntime,
    SeparateOsPrincipal,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlKeyCustodyClaim {
    NotClaimed,
    SoftwareBacked,
    HardwareBacked,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlGestureSourceClaim {
    NotClaimed,
    LocalInteractive,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlServerObservedCorroboration {
    None,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlProvenanceTier {
    SelfAsserted,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnerControlEnrollmentContext {
    PostgresRecordStore,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlHostPrincipalClaim {
    #[serde(default = "default_provenance_schema_version")]
    pub schema_version: u8,
    pub host_instance_id: String,
    pub principal_id: String,
    pub principal_separation: OwnerControlPrincipalSeparationClaim,
    pub key_custody: OwnerControlKeyCustodyClaim,
    pub gesture_source: OwnerControlGestureSourceClaim,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlEnrollmentProvenance {
    #[serde(default = "default_provenance_schema_version")]
    pub schema_version: u8,
    pub channel_session_id: String,
    pub owner_github_id: i64,
    pub binding_json: String,
    pub binding_sha256: String,
    pub host_principal_claim_json: String,
    pub host_principal_claim_sha256: String,
    pub enrolled_at: String,
    #[serde(default = "default_enrollment_context")]
    pub enrollment_context: OwnerControlEnrollmentContext,
    #[serde(default = "default_server_corroboration")]
    pub server_observed_corroboration: OwnerControlServerObservedCorroboration,
    #[serde(default = "default_provenance_tier")]
    pub provenance_tier: OwnerControlProvenanceTier,
    #[serde(default = "default_authority_state")]
    pub authority_state: OwnerControlAuthorityState,
    #[serde(default)]
    pub authorizes_execution: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerControlProvenanceResult {
    pub authority_state: OwnerControlAuthorityState,
    pub authorizes_execution: bool,
    pub provenance_tier: OwnerControlProvenanceTier,
    pub server_observed_corroboration: OwnerControlServerObservedCorroboration,
}

impl OwnerControlHostPrincipalClaim {
    pub fn from_value(value: serde_json::Value) -> Result<Self, ValidationError> {
        let claim: Self = deserialize_value(value)?;
        claim.validate()?;
        Ok(claim)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != OWNER_CONTROL_ENROLLMENT_PROVENANCE_SCHEMA_VERSION {
            return Err(ValidationError::Field {
                field: "schema_version",
                rule: "exactly 1",
            });
        }
        validate_opaque_identifier(&self.host_instance_id, "host_instance_id")?;
        validate_opaque_identifier(&self.principal_id, "principal_id")?;
        Ok(())
    }
}

impl OwnerControlEnrollmentProvenance {
    pub fn from_value(value: serde_json::Value) -> Result<Self, ValidationError> {
        let provenance: Self = deserialize_value(value)?;
        provenance.validate()?;
        Ok(provenance)
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != OWNER_CONTROL_ENROLLMENT_PROVENANCE_SCHEMA_VERSION {
            return Err(ValidationError::Field {
                field: "schema_version",
                rule: "exactly 1",
            });
        }
        if !(1..=i64::MAX).contains(&self.owner_github_id) {
            return Err(ValidationError::Field {
                field: "owner_github_id",
                rule: "a positive signed 64-bit integer",
            });
        }
        validate_json_length(&self.binding_json, "binding_json")?;
        validate_sha256(&self.binding_sha256, "binding_sha256")?;
        validate_json_length(&self.host_principal_claim_json, "host_principal_claim_json")?;
        validate_sha256(
            &self.host_principal_claim_sha256,
            "host_principal_claim_sha256",
        )?;

        let binding_value =
            serde_json::from_str(&self.binding_json).map_err(|source| ValidationError::Json {
                location: vec![ErrorLocation::Field("binding_json".to_owned())],
                source,
            })?;
        let binding = ChannelBindingRecord::from_value(binding_value)
            .map_err(|error| error.at_field("binding_json"))?;
        let binding_value =
            serde_json::to_value(&binding).map_err(|source| ValidationError::Json {
                location: Vec::new(),
                source,
            })?;
        if canonical_json_bytes(&binding_value)? != self.binding_json.as_bytes() {
            return Err(ValidationError::Rule {
                rule: "binding_json must contain exact canonical channel-binding bytes",
            });
        }
        if self.channel_session_id != binding.channel_session_id {
            return Err(ValidationError::Rule {
                rule: "channel_session_id must match the stored channel binding",
            });
        }
        if self.owner_github_id != binding.owner_github_id {
            return Err(ValidationError::Rule {
                rule: "owner_github_id must match the stored channel binding",
            });
        }
        if self.binding_sha256 != channel_binding_sha256(&binding)? {
            return Err(ValidationError::Rule {
                rule: "binding_sha256 must match the stored channel binding",
            });
        }

        let claim_value =
            serde_json::from_str(&self.host_principal_claim_json).map_err(|source| {
                ValidationError::Json {
                    location: vec![ErrorLocation::Field("host_principal_claim_json".to_owned())],
                    source,
                }
            })?;
        let claim = OwnerControlHostPrincipalClaim::from_value(claim_value)
            .map_err(|error| error.at_field("host_principal_claim_json"))?;
        let claim_value = serde_json::to_value(&claim).map_err(|source| ValidationError::Json {
            location: Vec::new(),
            source,
        })?;
        if canonical_json_bytes(&claim_value)? != self.host_principal_claim_json.as_bytes() {
            return Err(ValidationError::Rule {
                rule: "host_principal_claim_json must contain exact canonical claim bytes",
            });
        }
        if self.host_principal_claim_sha256 != owner_control_host_principal_claim_sha256(&claim)? {
            return Err(ValidationError::Rule {
                rule: "host_principal_claim_sha256 must match the stored claim",
            });
        }

        let enrolled_at = parse_timestamp(&self.enrolled_at, "enrolled_at")?;
        let issued_at = parse_timestamp(&binding.session_issued_at, "session_issued_at")?;
        let expires_at = parse_timestamp(&binding.session_expires_at, "session_expires_at")?;
        if enrolled_at < issued_at || enrolled_at > expires_at {
            return Err(ValidationError::Rule {
                rule: "enrolled_at must be inside the channel session interval",
            });
        }
        if self.provenance_tier
            != derive_owner_control_provenance_tier(self.server_observed_corroboration)
            || self.authority_state != OwnerControlAuthorityState::Inert
            || self.authorizes_execution
        {
            return Err(ValidationError::Rule {
                rule: "enrollment provenance must remain self-asserted and inert",
            });
        }
        Ok(())
    }

    pub fn channel_binding(&self) -> Result<ChannelBindingRecord, ValidationError> {
        let value =
            serde_json::from_str(&self.binding_json).map_err(|source| ValidationError::Json {
                location: vec![ErrorLocation::Field("binding_json".to_owned())],
                source,
            })?;
        ChannelBindingRecord::from_value(value)
    }

    pub fn host_principal_claim(&self) -> Result<OwnerControlHostPrincipalClaim, ValidationError> {
        let value = serde_json::from_str(&self.host_principal_claim_json).map_err(|source| {
            ValidationError::Json {
                location: vec![ErrorLocation::Field("host_principal_claim_json".to_owned())],
                source,
            }
        })?;
        OwnerControlHostPrincipalClaim::from_value(value)
    }
}

pub fn owner_control_host_principal_claim_sha256(
    claim: &OwnerControlHostPrincipalClaim,
) -> Result<String, ValidationError> {
    claim.validate()?;
    let value = serde_json::to_value(claim).map_err(|source| ValidationError::Json {
        location: Vec::new(),
        source,
    })?;
    Ok(canonical_json_sha256(&value)?)
}

pub fn derive_owner_control_provenance_tier(
    server_observed_corroboration: OwnerControlServerObservedCorroboration,
) -> OwnerControlProvenanceTier {
    match server_observed_corroboration {
        OwnerControlServerObservedCorroboration::None => OwnerControlProvenanceTier::SelfAsserted,
    }
}

pub fn owner_control_public_key_sha256(public_key: &str) -> Result<String, ValidationError> {
    let raw_key = URL_SAFE_NO_PAD
        .decode(public_key)
        .map_err(|_| ValidationError::Field {
            field: "owner_public_key",
            rule: "valid unpadded base64url",
        })?;
    if raw_key.len() != 32 {
        return Err(ValidationError::Field {
            field: "owner_public_key",
            rule: "exactly 32 decoded bytes",
        });
    }
    Ok(format!("{:x}", Sha256::digest(raw_key)))
}

pub fn is_published_owner_control_synthetic_public_key(
    public_key: &str,
) -> Result<bool, ValidationError> {
    let digest = owner_control_public_key_sha256(public_key)?;
    Ok(PUBLISHED_SYNTHETIC_PUBLIC_KEY_SHA256.contains(&digest.as_str()))
}

pub(crate) fn is_published_synthetic_public_key_sha256(digest: &str) -> bool {
    PUBLISHED_SYNTHETIC_PUBLIC_KEY_SHA256.contains(&digest)
}

fn default_provenance_schema_version() -> u8 {
    OWNER_CONTROL_ENROLLMENT_PROVENANCE_SCHEMA_VERSION
}

fn default_enrollment_context() -> OwnerControlEnrollmentContext {
    OwnerControlEnrollmentContext::PostgresRecordStore
}

fn default_server_corroboration() -> OwnerControlServerObservedCorroboration {
    OwnerControlServerObservedCorroboration::None
}

fn default_provenance_tier() -> OwnerControlProvenanceTier {
    OwnerControlProvenanceTier::SelfAsserted
}

fn default_authority_state() -> OwnerControlAuthorityState {
    OwnerControlAuthorityState::Inert
}

fn validate_opaque_identifier(value: &str, field: &'static str) -> Result<(), ValidationError> {
    let mut bytes = value.bytes();
    let valid = value.len() <= 256
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    if !valid {
        return Err(ValidationError::Field {
            field,
            rule: "a canonical opaque identifier",
        });
    }
    Ok(())
}

fn validate_json_length(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if !(2..=20_000).contains(&value.chars().count()) {
        return Err(ValidationError::Field {
            field,
            rule: "between 2 and 20000 characters",
        });
    }
    Ok(())
}

fn validate_sha256(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ValidationError::Field {
            field,
            rule: "a lowercase SHA-256 digest",
        });
    }
    Ok(())
}
